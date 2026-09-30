//! P14 — Windows ACL journaling primitives for the AppContainer sandbox:
//! capture self-relative security descriptors, plan canonical grant
//! deltas, apply them, read back ActualAfter and restore Before only by
//! physical/ACL compare-and-swap. No mutation happens without a persisted
//! prepared operation; an external edit settles as Conflict and is never
//! overwritten. This module owns Win32 mechanics only — composing it into
//! an AppContainer launch is P15.

#![cfg(windows)]

use r_code_store::v1::safety::{AclOperationRecord, AclOperationState};
use windows_sys::Win32::Foundation::{LocalFree, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    AddAccessAllowedAceEx, AddAce, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorLength,
    InitializeAcl, IsValidSecurityDescriptor, MakeSelfRelativeSD, ACCESS_ALLOWED_ACE, ACE_REVISION,
    ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    GROUP_SECURITY_INFORMATION, INHERITED_ACE, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_SELF_RELATIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, SYNCHRONIZE, WRITE_DAC};

const ACL_REVISION_DS: ACE_REVISION = 4;
const ACL_SIZE_INFORMATION_CLASS: i32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AclError {
    #[error("windows ACL API failed at {step}: code {code}")]
    Api { step: &'static str, code: i32 },
    #[error("acl CAS mismatch: {0}")]
    CasMismatch(&'static str),
}

fn api_error(step: &'static str) -> AclError {
    AclError::Api {
        step,
        code: unsafe { windows_sys::Win32::Foundation::GetLastError() as i32 },
    }
}

/// One explicit (non-inherited) ACE, extracted for canonical comparison.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ExplicitAce {
    pub ace_type: u8,
    pub ace_flags: u8,
    pub access_mask: u32,
    pub sid: Vec<u8>,
}

/// Everything one journal decision needs from a target: the canonical
/// self-relative descriptor bytes plus the extracted explicit-ACE set
/// (inherited ACEs live only in the bytes; classification never trusts
/// their ordering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedAcl {
    pub self_relative: Vec<u8>,
    pub explicit_aces: Vec<ExplicitAce>,
}

/// One planned grant (P14.1): an allow ACE for one SID/mask. The delta is
/// the SORTED canonical set, so equal grants always serialize identically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct PlannedGrant {
    pub sid: Vec<u8>,
    pub access_mask: u32,
    pub ace_flags: u8,
}

impl PlannedGrant {
    pub fn subtree_grant(sid: Vec<u8>, access_mask: u32) -> Self {
        Self {
            sid,
            access_mask,
            ace_flags: (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8,
        }
    }
}

/// Canonical PlannedDelta JSON (sorted grants; identical content always
/// serializes identically, so the journal stores comparable bytes).
pub fn canonical_planned_delta(grants: &[PlannedGrant]) -> String {
    let mut sorted = grants.to_vec();
    sorted.sort();
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn wide(path: &str) -> Vec<u16> {
    path.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_target(path: &str, desired_access: u32) -> Result<HANDLE, AclError> {
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(api_error("CreateFileW"));
    }
    Ok(handle)
}

fn close_handle(handle: HANDLE) {
    // SAFETY: handle ownership ends here; CloseHandle only closes it.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
}

/// Stable physical identity of the target (P14.1): volume serial plus the
/// 128-bit file id — rename-proof and pid-independent. Read-only.
pub fn physical_file_identity(path: &str) -> Result<String, AclError> {
    let handle = open_target(path, FILE_READ_ATTRIBUTES)?;
    let mut info = FILE_ID_INFO::default();
    // SAFETY: info has exactly the documented size; the return value is
    // nonzero on success.
    let written = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut info as *mut FILE_ID_INFO as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    close_handle(handle);
    if written == 0 {
        return Err(api_error("GetFileInformationByHandleEx"));
    }
    Ok(format!(
        "vol-{}-fileid-{}",
        info.VolumeSerialNumber,
        info.FileId
            .Identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// Extract the explicit (non-inherited, allow-type) ACE set from a
/// self-relative descriptor. Inherited ACEs are skipped: classification
/// compares explicit sets only, so inherited reordering never fakes a
/// conflict and never fakes equality either.
fn extract_explicit_aces(descriptor: &[u8]) -> Vec<ExplicitAce> {
    let mut aces = Vec::new();
    let mut dacl_present = 0i32;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0i32;
    // SAFETY: descriptor is a valid self-relative SD (MakeSelfRelativeSD
    // produced it); the out-pointers are plain stack slots.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.as_ptr() as PSECURITY_DESCRIPTOR,
            &mut dacl_present,
            &mut dacl,
            &mut defaulted,
        );
    }
    if dacl_present == 0 || dacl.is_null() {
        return aces;
    }
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: dacl came from the descriptor above; size is a plain slot.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            &mut size as *mut ACL_SIZE_INFORMATION as *mut core::ffi::c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            ACL_SIZE_INFORMATION_CLASS,
        )
    };
    if ok == 0 {
        return aces;
    }
    for index in 0..size.AceCount {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: index is within the AceCount reported above.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            continue;
        }
        // SAFETY: every ACE begins with ACE_HEADER { AceType, AceFlags,
        // AceSize } — reading the two type/flag bytes is always in bounds.
        let header = ace as *const u8;
        let ace_type = unsafe { *header };
        let ace_flags = unsafe { *header.add(1) };
        if ace_type != 0 /* ACCESS_ALLOWED_ACE_TYPE */ || (ace_flags & INHERITED_ACE as u8) != 0 {
            continue;
        }
        // SAFETY: an ACCESS_ALLOWED_ACE is header + mask + SID; the SID
        // starts after the fixed prefix and GetLengthSid bounds it.
        let allowed = ace as *const ACCESS_ALLOWED_ACE;
        let access_mask = unsafe { (*allowed).Mask };
        let sid = unsafe { header.add(std::mem::size_of::<ACCESS_ALLOWED_ACE>() - 4) };
        // SAFETY: IsValidSid/GetLengthSid only read the SID header.
        if unsafe { windows_sys::Win32::Security::IsValidSid(sid as PSID) } == 0 {
            continue;
        }
        let sid_len = unsafe { GetLengthSid(sid as PSID) } as usize;
        let sid_bytes = unsafe { std::slice::from_raw_parts(sid, sid_len) };
        aces.push(ExplicitAce {
            ace_type,
            ace_flags,
            access_mask,
            sid: sid_bytes.to_vec(),
        });
    }
    aces.sort();
    aces
}

fn get_security_info(
    handle: HANDLE,
    info: windows_sys::Win32::Security::OBJECT_SECURITY_INFORMATION,
) -> Result<*mut core::ffi::c_void, AclError> {
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut ignored_sid: PSID = std::ptr::null_mut();
    let mut ignored_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: handle is owned by the caller; descriptor is LocalAlloc'd by
    // the API and handed back for the caller to LocalFree.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            info,
            &mut ignored_sid, // owner (re-read separately when restoring)
            &mut ignored_sid, // group
            &mut ignored_acl, // dacl
            &mut ignored_acl, // sacl
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(api_error("GetSecurityInfo"));
    }
    Ok(descriptor)
}

/// Capture the current security state (P14.1): one GetSecurityInfo call
/// (owner+group+DACL), converted to canonical self-relative bytes, plus
/// the extracted explicit-ACE set. GetSecurityInfo already hands back a
/// self-relative descriptor, so the self-relative conversion below only
/// runs for absolute inputs (defensive; both forms are valid here).
pub fn capture_acl(path: &str) -> Result<CapturedAcl, AclError> {
    let handle = open_target(path, READ_CONTROL | SYNCHRONIZE)?;
    let captured = (|| {
        let descriptor = get_security_info(
            handle,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        )?;
        // SAFETY: descriptor is a valid SD from the API above and freed
        // exactly once on every path.
        let bytes = unsafe {
            let valid = IsValidSecurityDescriptor(descriptor);
            if valid == 0 {
                LocalFree(descriptor);
                return Err(api_error("IsValidSecurityDescriptor"));
            }
            let mut control: u16 = 0;
            let mut revision: u32 = 0;
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision);
            let copied = if control & SE_SELF_RELATIVE != 0 {
                let length = GetSecurityDescriptorLength(descriptor) as usize;
                std::slice::from_raw_parts(descriptor as *const u8, length).to_vec()
            } else {
                let mut size = 0u32;
                MakeSelfRelativeSD(descriptor, std::ptr::null_mut(), &mut size);
                let mut buffer = vec![0u8; size as usize];
                let written = MakeSelfRelativeSD(
                    descriptor,
                    buffer.as_mut_ptr() as *mut core::ffi::c_void,
                    &mut size,
                );
                if written == 0 {
                    LocalFree(descriptor);
                    return Err(api_error("MakeSelfRelativeSD"));
                }
                buffer.truncate(size as usize);
                buffer
            };
            LocalFree(descriptor);
            copied
        };
        let explicit_aces = extract_explicit_aces(&bytes);
        Ok(CapturedAcl {
            self_relative: bytes,
            explicit_aces,
        })
    })();
    close_handle(handle);
    captured
}

/// Apply the planned grants (P14.2): build a new DACL from the CURRENT
/// explicit ACEs (verbatim, non-protected — the OS continues to own the
/// inherited portion) plus the new allow ACEs appended last, so an
/// existing deny keeps winning. The caller reads back ActualAfter
/// immediately after this returns.
pub fn apply_planned_grants(path: &str, grants: &[PlannedGrant]) -> Result<(), AclError> {
    if grants.is_empty() {
        return Err(AclError::CasMismatch("planned delta is empty"));
    }
    let handle = open_target(path, READ_CONTROL | WRITE_DAC | SYNCHRONIZE)?;
    let applied = unsafe { apply_with_handle(handle, grants) };
    close_handle(handle);
    applied
}

/// SAFETY: handle must be open with READ_CONTROL|WRITE_DAC and live for
/// the whole call.
unsafe fn apply_with_handle(handle: HANDLE, grants: &[PlannedGrant]) -> Result<(), AclError> {
    let descriptor = get_security_info(handle, DACL_SECURITY_INFORMATION)?;
    let outcome = (|| {
        let mut dacl_present = 0i32;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut defaulted = 0i32;
        GetSecurityDescriptorDacl(
            descriptor as PSECURITY_DESCRIPTOR,
            &mut dacl_present,
            &mut dacl,
            &mut defaulted,
        );
        let mut size = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            dacl,
            &mut size as *mut ACL_SIZE_INFORMATION as *mut core::ffi::c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            ACL_SIZE_INFORMATION_CLASS,
        );
        let grant_bytes: usize = grants
            .iter()
            .map(|grant| std::mem::size_of::<ACCESS_ALLOWED_ACE>() - 4 + grant.sid.len())
            .sum();
        let new_capacity = size.AclBytesInUse as usize + grant_bytes + 64;
        let mut new_acl_bytes = vec![0u8; new_capacity];
        let new_acl = new_acl_bytes.as_mut_ptr() as *mut ACL;
        if InitializeAcl(new_acl, new_capacity as u32, ACL_REVISION_DS) == 0 {
            return Err(api_error("InitializeAcl"));
        }
        // Re-add every currently explicit ACE verbatim (INHERITED_ACE
        // cleared): the OS re-derives the inherited portion because the
        // DACL stays non-protected.
        for index in 0..size.AceCount {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                continue;
            }
            let header = ace as *const u8;
            let ace_flags = *header.add(1);
            if (ace_flags & INHERITED_ACE as u8) != 0 {
                continue;
            }
            let ace_size = *(header as *const u16).add(1) as usize;
            let mut ace_copy = std::slice::from_raw_parts(header, ace_size).to_vec();
            ace_copy[1] = ace_flags & !(INHERITED_ACE as u8);
            if AddAce(
                new_acl,
                ACL_REVISION_DS,
                u32::MAX,
                ace_copy.as_ptr() as *const core::ffi::c_void,
                ace_size as u32,
            ) == 0
            {
                return Err(api_error("AddAce"));
            }
        }
        for grant in grants {
            if AddAccessAllowedAceEx(
                new_acl,
                ACL_REVISION_DS,
                grant.ace_flags as u32,
                grant.access_mask,
                grant.sid.as_ptr() as PSID,
            ) == 0
            {
                return Err(api_error("AddAccessAllowedAceEx"));
            }
        }
        let status = SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_acl,
            std::ptr::null_mut(),
        );
        if status != 0 {
            return Err(api_error("SetSecurityInfo"));
        }
        Ok(())
    })();
    LocalFree(descriptor);
    outcome
}

/// P14.2 classification of a crash-recovered Prepared operation by
/// comparing the CURRENT capture with the persisted Before and delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedRecovery {
    /// Nothing was applied: the current explicit set equals Before's.
    NothingApplied,
    /// The exact delta landed but the readback never happened: the current
    /// explicit set equals Before ∪ grants.
    AlreadyApplied,
    /// Anything else — an external edit, preserved as-is.
    Conflict(&'static str),
}

pub fn classify_prepared_recovery(
    before: &CapturedAcl,
    grants: &[PlannedGrant],
    current: &CapturedAcl,
) -> PreparedRecovery {
    let mut before_set = before.explicit_aces.clone();
    before_set.sort();
    let mut current_set = current.explicit_aces.clone();
    current_set.sort();
    if current_set == before_set {
        return PreparedRecovery::NothingApplied;
    }
    // SID-level matching: the OS canonicalizes an inheritable grant ACE on
    // write (mask rewrite plus an effective/inherit-only split), so the
    // classification tolerates flag/mask drift on the GRANTED SIDs only —
    // every other added or modified ACE stays a conflict, and every grant
    // must have landed at least once. Restore remains byte-CAS against the
    // journaled descriptor, so this tolerance never weakens the CAS.
    let granted_sids: std::collections::BTreeSet<&Vec<u8>> =
        grants.iter().map(|grant| &grant.sid).collect();
    let mut saw_grant = false;
    for ace in &current_set {
        if before_set.contains(ace) {
            continue;
        }
        if ace.ace_type == 0 && granted_sids.contains(&ace.sid) {
            saw_grant = true;
            continue;
        }
        return PreparedRecovery::Conflict("external-change-during-prepare");
    }
    let every_grant_landed = grants
        .iter()
        .all(|grant| current_set.iter().any(|ace| ace.sid == grant.sid));
    if saw_grant && every_grant_landed {
        PreparedRecovery::AlreadyApplied
    } else {
        PreparedRecovery::Conflict("external-change-during-prepare")
    }
}

/// P14.3 restore: reinstall the Before descriptor ONLY when the physical
/// identity still matches AND the current self-relative bytes equal the
/// journaled comparison basis (ActualAfter for Applied operations). An
/// external edit returns Err(reason) and is never overwritten.
pub fn restore_before(
    path: &str,
    record: &AclOperationRecord,
) -> Result<Result<(), &'static str>, AclError> {
    if physical_file_identity(path)? != record.physical_identity {
        return Ok(Err("physical-identity-changed"));
    }
    let basis = match record.state {
        AclOperationState::Applied => record
            .actual_after
            .clone()
            .ok_or(AclError::CasMismatch("applied record lacks actual-after"))?,
        _ => return Err(AclError::CasMismatch("restore requires an applied record")),
    };
    let current = capture_acl(path)?;
    if current.self_relative != basis {
        return Ok(Err("acl-changed-since-apply"));
    }
    // Only the DACL is restored: apply mutated the DACL alone, so the
    // restore needs WRITE_DAC (not WRITE_OWNER — volumes granting only
    // Modify to the host user make owner rewrites impossible, and an
    // owner rewrite we never needed would fail the whole CAS).
    let handle = open_target(path, READ_CONTROL | WRITE_DAC | SYNCHRONIZE)?;
    let before = record.before_descriptor.as_slice();
    let restored = unsafe {
        let mut dacl_present = 0i32;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted = 0i32;
        // SAFETY: all reads target the journaled self-relative Before
        // bytes, which capture_acl produced and the journal stored.
        GetSecurityDescriptorDacl(
            before.as_ptr() as PSECURITY_DESCRIPTOR,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        );
        // SAFETY: the DACL pointer came from the stored descriptor; the
        // call reinstalls exactly the DACL that was journaled.
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        )
    };
    close_handle(handle);
    if restored != 0 {
        return Err(api_error("SetSecurityInfo"));
    }
    let readback = capture_acl(path)?;
    if readback.self_relative != record.before_descriptor {
        return Ok(Err("restore-readback-mismatch"));
    }
    Ok(Ok(()))
}

/// Build a prepared record (P14.1) for one target and grant set; the
/// caller persists it BEFORE calling [`apply_planned_grants`].
pub fn build_prepared_operation(
    operation_id: String,
    path: &str,
    grants: &[PlannedGrant],
    prepared_at_ms: i64,
) -> Result<AclOperationRecord, AclError> {
    let identity = physical_file_identity(path)?;
    let before = capture_acl(path)?;
    Ok(AclOperationRecord {
        operation_id,
        target_path: path.to_string(),
        physical_identity: identity,
        state: AclOperationState::Prepared,
        before_descriptor: before.self_relative,
        planned_delta: canonical_planned_delta(grants),
        actual_after: None,
        conflict_reason: None,
        prepared_at_ms,
        applied_at_ms: None,
        settled_at_ms: None,
    })
}

// P15 — Windows AppContainer policy and launch ------------------------------

use r_code_store::v1::V1Store;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{CopySid, DeriveCapabilitySidsFromName, FreeSid};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};

/// RAII owner of one registered AppContainer profile: Drop deletes it, so
/// a launch (or a failed launch) never leaks a profile registration.
pub struct AppContainerProfileGuard {
    name: String,
}

impl AppContainerProfileGuard {
    /// Register a profile and return it with the profile SID bytes.
    /// Capabilities are derived separately (`internetClient` only for
    /// PublicInternetClient — P15.2); the profile itself starts bare.
    pub fn create(name: &str) -> Result<(Self, Vec<u8>), AclError> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: wide is NUL-terminated; sid is an out-slot freed below on
        // every path (profile SIDs from this API are LocalAlloc'd).
        let status = unsafe {
            CreateAppContainerProfile(
                wide.as_ptr(),
                wide.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if status != 0 {
            return Err(AclError::Api {
                step: "CreateAppContainerProfile",
                code: status,
            });
        }
        // SAFETY: sid is valid on success; copy the bytes out and free the
        // original.
        let bytes = unsafe {
            let length = GetLengthSid(sid) as usize;
            let mut buffer = vec![0u8; length];
            let copied = CopySid(length as u32, buffer.as_mut_ptr() as PSID, sid);
            FreeSid(sid);
            if copied == 0 {
                return Err(AclError::Api {
                    step: "CopySid",
                    code: 0,
                });
            }
            buffer
        };
        Ok((Self { name: name.into() }, bytes))
    }
}

impl Drop for AppContainerProfileGuard {
    fn drop(&mut self) {
        let wide: Vec<u16> = self.name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide is NUL-terminated; deletion is the guard's purpose.
        unsafe { DeleteAppContainerProfile(wide.as_ptr()) };
    }
}

/// Derive the SID bytes for one capability name (P15.2). The group SIDs
/// are freed; only the capability SIDs are kept for the token.
pub fn derive_capability_sids(capability: &str) -> Result<Vec<Vec<u8>>, AclError> {
    let wide: Vec<u16> = capability
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut group_sids: *mut PSID = std::ptr::null_mut();
    let mut group_count = 0u32;
    let mut capability_sids: *mut PSID = std::ptr::null_mut();
    let mut capability_count = 0u32;
    // SAFETY: all out-slots are plain stack locals; the arrays are
    // LocalAlloc'd by the API and freed below on every path.
    let ok = unsafe {
        DeriveCapabilitySidsFromName(
            wide.as_ptr(),
            &mut group_sids,
            &mut group_count,
            &mut capability_sids,
            &mut capability_count,
        )
    };
    if ok == 0 {
        return Err(AclError::Api {
            step: "DeriveCapabilitySidsFromName",
            code: unsafe { windows_sys::Win32::Foundation::GetLastError() as i32 },
        });
    }
    let mut derived = Vec::new();
    unsafe {
        for index in 0..capability_count as usize {
            let sid = *capability_sids.add(index);
            if sid.is_null() {
                continue;
            }
            let length = GetLengthSid(sid) as usize;
            let mut buffer = vec![0u8; length];
            if CopySid(length as u32, buffer.as_mut_ptr() as PSID, sid) != 0 {
                derived.push(buffer);
            }
            FreeSid(sid);
        }
        for index in 0..group_count as usize {
            let sid = *group_sids.add(index);
            if !sid.is_null() {
                FreeSid(sid);
            }
        }
        if !capability_sids.is_null() {
            windows_sys::Win32::Foundation::LocalFree(capability_sids as *mut core::ffi::c_void);
        }
        if !group_sids.is_null() {
            windows_sys::Win32::Foundation::LocalFree(group_sids as *mut core::ffi::c_void);
        }
    }
    if derived.is_empty() {
        return Err(AclError::CasMismatch(
            "capability derivation produced no SIDs",
        ));
    }
    Ok(derived)
}

/// One ACL grant in the launch plan (P15.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppContainerGrant {
    pub path: PathBuf,
    /// Subtree grants carry OI|CI; single-file grants carry none.
    pub subtree: bool,
}

/// The complete AppContainer launch plan for one sandbox profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppContainerLaunchPlan {
    pub capability_sids: Vec<Vec<u8>>,
    pub grants: Vec<AppContainerGrant>,
}

/// Build the launch plan (P15.1/P15.2): capability SIDs from the network
/// class (Offline → none; PublicInternetClient → internetClient only;
/// HostNetwork → unsupported) and the explicit ACL grant set — every
/// profile root becomes exactly one subtree grant and NOTHING else
/// (SeChangeNotifyPrivilege carries traversal through ungranted
/// ancestors). A path that is or contains a `.git` component is refused:
/// the sandbox NEVER grants .git, and nothing recursive is ever implied
/// beyond the listed roots.
pub fn build_appcontainer_launch_plan(
    profile: &super::SandboxProfileMaterial,
) -> Result<AppContainerLaunchPlan, String> {
    profile.validate().map_err(|reason| reason.to_string())?;
    let capability_sids = match profile.network {
        super::SandboxNetworkClass::Offline => Vec::new(),
        super::SandboxNetworkClass::PublicInternetClient => {
            derive_capability_sids("internetClient").map_err(|error| error.to_string())?
        }
        super::SandboxNetworkClass::HostNetwork => {
            return Err("host-network is unsupported on windows appcontainer".into());
        }
    };
    let mut granted: Vec<PathBuf> = Vec::new();
    for root in profile
        .read_roots
        .iter()
        .chain(profile.toolchain_roots.iter())
        .chain(profile.cache_roots.iter())
        .chain(profile.write_roots.iter())
        .chain([&profile.scratch_root])
    {
        let path = Path::new(root);
        if path
            .components()
            .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))
        {
            return Err("appcontainer grants must never cover .git".into());
        }
        granted.push(path.to_path_buf());
    }
    granted.sort();
    granted.dedup();
    // Deliberately NO ancestor-traversal grants: every normal process
    // holds SeChangeNotifyPrivilege (bypass-traverse-checking), so an
    // AppContainer reaches granted paths through ungranted ancestors
    // without any ACL change on them — empirically verified. Granting
    // shared ancestors (e.g. C:\Users) would need WRITE_DAC the host
    // does not have and would smear an unrestorable inherited ACE onto
    // every pre-existing descendant.
    let grants: Vec<AppContainerGrant> = granted
        .into_iter()
        .map(|target| AppContainerGrant {
            path: target,
            subtree: true,
        })
        .collect();
    Ok(AppContainerLaunchPlan {
        capability_sids,
        grants,
    })
}

/// Grants for one target as P14 planned grants.
fn planned_grants_for(subtree: bool, app_container_sid: &[u8]) -> Vec<PlannedGrant> {
    let mask = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | FILE_GENERIC_WRITE;
    if subtree {
        vec![PlannedGrant::subtree_grant(
            app_container_sid.to_vec(),
            mask,
        )]
    } else {
        vec![PlannedGrant {
            sid: app_container_sid.to_vec(),
            access_mask: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            ace_flags: 0,
        }]
    }
}

/// The Windows AppContainer sandbox backend (P15). Construction requires
/// the V1Store because every ACL mutation is journaled (P14 law); probes
/// run the REAL helper binary inside a creation-time Job+lowbox and the
/// ACL set is reconciled after the tree is proven dead.
pub struct WindowsAppContainerBackend {
    store: Arc<V1Store>,
}

impl WindowsAppContainerBackend {
    pub fn new(store: Arc<V1Store>) -> Self {
        Self { store }
    }

    /// The probe set this backend requires for activation: every deny
    /// probe the lowbox must enforce. Child spawning is deliberately NOT
    /// required on Windows — containment is the Job's contract (P07),
    /// proven separately, not a fork denial.
    pub fn required_probes(&self) -> &'static [super::SandboxProbeId] {
        &[
            super::SandboxProbeId::WriteOutsideAllowlist,
            super::SandboxProbeId::DotGitAccess,
            super::SandboxProbeId::NetworkOutbound,
            super::SandboxProbeId::RegistryOrKeychain,
            super::SandboxProbeId::DeviceAccess,
            super::SandboxProbeId::IpcAccess,
            super::SandboxProbeId::EnvironmentLeakage,
        ]
    }
}

/// Helper identity (path + sha256) for report material binding (P15.3).
pub fn probe_helper_identity(path: &str) -> Result<super::SafetyBinaryIdentity, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    Ok(super::SafetyBinaryIdentity {
        path: path.to_string(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

#[async_trait::async_trait]
impl super::SandboxBackend for WindowsAppContainerBackend {
    fn id(&self) -> &'static str {
        "windows-appcontainer"
    }

    fn policy_digest(&self, profile: &super::SandboxProfileMaterial) -> Result<String, String> {
        let plan = build_appcontainer_launch_plan(profile)?;
        Ok(r_code_harness_protocol::canonical_input_hash(
            &serde_json::json!({
                "backend": "windows-appcontainer",
                "profile": profile.policy_digest(),
                "capabilitySids": plan.capability_sids,
                "grants": plan.grants.iter().map(|grant| serde_json::json!({
                    "path": grant.path.to_string_lossy(),
                    "subtree": grant.subtree,
                })).collect::<Vec<_>>(),
            }),
        ))
    }

    async fn run_probes(
        &self,
        helper: &super::SafetyBinaryIdentity,
        profile: &super::SandboxProfileMaterial,
        probes: &[super::SandboxProbeId],
    ) -> Result<Vec<super::SafetyProbeResult>, String> {
        run_appcontainer_probes(&self.store, helper, profile, probes).await
    }
}

/// The full probe ceremony (P15.3): plan → journal → lowbox launch →
/// helper execution → tree-death proof → ACL reconcile. Any failure before
/// the helper runs still reconciles the ACL set — failure is SafeDisabled
/// with recovery, never a leaked grant.
async fn run_appcontainer_probes(
    store: &V1Store,
    helper: &super::SafetyBinaryIdentity,
    profile: &super::SandboxProfileMaterial,
    probes: &[super::SandboxProbeId],
) -> Result<Vec<super::SafetyProbeResult>, String> {
    let plan = build_appcontainer_launch_plan(profile)?;
    let scratch = Path::new(&profile.scratch_root);
    std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
    let profile_name = format!(
        "r-code-sandbox-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let (guard, app_container_sid) =
        AppContainerProfileGuard::create(&profile_name).map_err(|e| e.to_string())?;
    let lowbox = crate::process_guard::windows::LowboxCapabilities {
        app_container_sid: app_container_sid.clone(),
        capability_sids: plan.capability_sids.clone(),
    };

    // Probe targets: everything the lowbox must DENY lives OUTSIDE the
    // grant set (the scratch IS granted, so deny targets use an
    // independent ungranted directory; OS propagation of inheritable
    // grants to pre-existing children makes any in-scratch target a
    // false "allowed").
    let deny_root =
        std::env::temp_dir().join(format!("r-code-s15-deny-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&deny_root).map_err(|error| error.to_string())?;
    let outside_write_path = deny_root.join("outside-sentinel.txt");
    let dot_git_dir = deny_root.join("probe.git");
    std::fs::create_dir_all(&dot_git_dir).map_err(|error| error.to_string())?;
    std::fs::write(dot_git_dir.join("HEAD"), b"ref").map_err(|error| error.to_string())?;
    // The network target is a REAL public-internet endpoint: inside an
    // AppContainer without the internetClient capability the firewall
    // denies the connect with WSAEACCES (empirically verified), which the
    // helper classifies as `denied`; an unsandboxed machine with outbound
    // access genuinely connects (`allowed`, failing the suite); an
    // offline environment yields `ambiguous`, which also fails the suite
    // — an unprovable network denial never activates anything. No
    // listener is bound here (the F2 pin keeps runtime listening
    // confined to remote/listener.rs).
    let network_target = ("example.com", 80)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.find(|address| address.is_ipv4()));
    let credential_service = format!("r-code-s15-probe-{}", uuid::Uuid::new_v4().simple());
    let planted_credential = {
        let entry = keyring::Entry::new(&credential_service, "sentinel")
            .map_err(|error| error.to_string())?;
        match entry.set_password("s15-sentinel") {
            Ok(()) => Some(entry),
            Err(_) => None, // planting failure makes that probe ambiguous
        }
    };
    let ipc_path = format!(
        "\\\\.\\pipe\\r-code-s15-probe-{}",
        uuid::Uuid::new_v4().simple()
    );
    let pipe_guard = create_probe_pipe(&ipc_path);

    // Journal + apply the ACL grants (P14 discipline), then launch. Every
    // failure below reconciles what was already applied — no unsettled
    // prepared row is ever left behind.
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0)
    };
    let mut operations: Vec<(String, PathBuf)> = Vec::new();
    let granted = (|| -> Result<(), String> {
        for (index, grant) in plan.grants.iter().enumerate() {
            let grants = planned_grants_for(grant.subtree, &app_container_sid);
            if grants.is_empty() {
                continue;
            }
            let path_text = grant.path.to_string_lossy().into_owned();
            let operation_id = format!("acl-{}-{index}", uuid::Uuid::new_v4().simple());
            let record = build_prepared_operation(operation_id.clone(), &path_text, &grants, now())
                .map_err(|error| error.to_string())?;
            store
                .prepare_acl_operation(record)
                .map_err(|error| error.to_string())?;
            apply_planned_grants(&path_text, &grants).map_err(|error| error.to_string())?;
            let readback = capture_acl(&path_text).map_err(|error| error.to_string())?;
            store
                .mark_acl_applied(&operation_id, readback.self_relative, now())
                .map_err(|error| error.to_string())?;
            operations.push((operation_id, grant.path.clone()));
        }
        Ok(())
    })();
    if let Err(error) = granted {
        reconcile_acl_operations(store, &operations);
        return Err(error);
    }

    // Helper request: exactly the requested probes with their targets.
    // A DNS resolution failure (offline host) reports the network probe
    // as failed rather than aborting the whole ceremony: every other
    // probe stays valid evidence, and the suite still cannot activate.
    let (network_host, network_port) = network_target
        .map(|address| (address.ip().to_string(), address.port()))
        .unwrap_or(("203.0.113.0".to_string(), 80u16));
    let network_resolved = network_target.is_some();
    let probe_ids: Vec<&str> = probes.iter().map(|probe| probe.as_str()).collect();
    let request = serde_json::json!({
        "version": 1,
        "probes": probe_ids,
        "targets": {
            "outsideWritePath": outside_write_path.to_string_lossy(),
            "dotGitDir": dot_git_dir.to_string_lossy(),
            "networkHost": network_host,
            "networkPort": network_port,
            "credentialService": credential_service,
            "credentialUser": "sentinel",
            "devicePath": "\\\\.\\PhysicalDrive0",
            "ipcPath": ipc_path,
            "childCommand": "cmd.exe",
            "forbiddenEnv": ["R_CODE_SANDBOX_FORBIDDEN_SENTINEL"],
        },
    });
    let environment = std::collections::BTreeMap::new();
    let spec = crate::process_guard::windows::RawSpawnSpec {
        executable: Path::new(&helper.path),
        arguments: &[],
        cwd: Some(scratch),
        environment: &environment,
    };
    let mut child = crate::process_guard::windows::spawn_suspended_with_job_lowbox(&spec, &lowbox)
        .map_err(|error| {
            reconcile_acl_operations(store, &operations);
            error.to_string()
        })?;
    let request_bytes = format!("{request}\n");
    let mut written = child
        .write_stdin(request_bytes.as_bytes())
        .map_err(|error| {
            reconcile_acl_operations(store, &operations);
            error.to_string()
        })?;
    while written < request_bytes.len() {
        let more = child
            .write_stdin(&request_bytes.as_bytes()[written..])
            .map_err(|error| {
                reconcile_acl_operations(store, &operations);
                error.to_string()
            })?;
        written += more;
    }
    child.close_stdin();
    child.resume_once().map_err(|error| {
        reconcile_acl_operations(store, &operations);
        error.to_string()
    })?;

    // Drain stdout until the child dies.
    let mut output: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let mut buffer = [0u8; 4096];
        match child.read_stdout(&mut buffer) {
            Ok(0) => break,
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(_) => break, // broken pipe: child exited
        }
        if std::time::Instant::now() >= deadline || child.primary_exited() {
            break;
        }
    }
    let proved = child.wait_all_members(deadline);

    // P15.3 reconcile: only after the tree is proven dead do the ACLs go
    // back; an unproven tree leaves the conflicts visible.
    reconcile_acl_operations(store, &operations);
    drop(guard);
    if let Some(entry) = planted_credential.as_ref() {
        // The sentinel is host state: delete it, never just drop the handle.
        let _ = entry.delete_credential();
    }
    drop(planted_credential);
    drop(pipe_guard);
    let _ = std::fs::remove_dir_all(&deny_root);
    if !proved {
        return Err("probe tree death could not be proven".into());
    }

    let parsed: serde_json::Value =
        serde_json::from_slice(&output).map_err(|error| error.to_string())?;
    if parsed.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("probe helper spoke an unknown protocol version".into());
    }
    if let Some(error) = parsed.get("error").and_then(serde_json::Value::as_str) {
        return Err(format!("probe helper refused: {error}"));
    }
    let empty = Vec::new();
    let results_json = parsed
        .get("results")
        .and_then(serde_json::Value::as_array)
        .unwrap_or(&empty);
    Ok(probes
        .iter()
        .map(|probe| {
            let entry = results_json.iter().find(|value| {
                value.get("probe").and_then(serde_json::Value::as_str) == Some(probe.as_str())
            });
            match entry {
                Some(value) => {
                    let denied =
                        value.get("outcome").and_then(serde_json::Value::as_str) == Some("denied");
                    let detail = value
                        .get("detail")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    // A DNS-resolution failure upstream means the network
                    // target was a placeholder: the probe cannot count.
                    let resolvable =
                        network_resolved || *probe != super::SandboxProbeId::NetworkOutbound;
                    super::SafetyProbeResult {
                        probe_id: probe.as_str().to_string(),
                        passed: denied && resolvable,
                        detail_digest: r_code_harness_protocol::canonical_input_hash(
                            &serde_json::json!(detail),
                        ),
                    }
                }
                // Missing result: the suite treats it as failed, never
                // as a pass (omission cannot satisfy activation).
                None => super::SafetyProbeResult {
                    probe_id: probe.as_str().to_string(),
                    passed: false,
                    detail_digest: r_code_harness_protocol::canonical_input_hash(
                        &serde_json::json!("probe-missing"),
                    ),
                },
            }
        })
        .collect())
}

/// Reconcile (P15.3): restore every journaled grant operation after the
/// probe ceremony, settling restored or conflict accordingly. A target
/// that vanished settles as conflict (visible, never silent).
fn reconcile_acl_operations(store: &V1Store, operations: &[(String, PathBuf)]) {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0)
    };
    for (operation_id, path) in operations {
        let Ok(Some(record)) = store.load_acl_operation(operation_id) else {
            continue;
        };
        match restore_before(&path.to_string_lossy(), &record) {
            Ok(Ok(())) => {
                let _ = store.settle_acl_operation(operation_id, true, None, now());
            }
            Ok(Err(reason)) => {
                let _ = store.settle_acl_operation(operation_id, false, Some(reason), now());
            }
            Err(error) => {
                let _ = store.settle_acl_operation(
                    operation_id,
                    false,
                    Some(&error.to_string()),
                    now(),
                );
            }
        }
    }
}

/// RAII owner of the probe pipe handle (Send: a raw handle value travels
/// across the async boundary; only this guard closes it).
struct ProbePipeGuard(Option<HANDLE>);
unsafe impl Send for ProbePipeGuard {}
impl Drop for ProbePipeGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            // SAFETY: the handle was created by CreateNamedPipeW and is
            // closed exactly once here.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        }
    }
}

/// Create one host-side named pipe instance for the IPC probe. The lowbox
/// child cannot reach the host's global pipe namespace without a grant,
/// so a live pipe still reads as denied inside the AppContainer.
fn create_probe_pipe(path: &str) -> ProbePipeGuard {
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
    use windows_sys::Win32::System::Pipes::CreateNamedPipeW;
    use windows_sys::Win32::System::Pipes::PIPE_READMODE_BYTE;
    use windows_sys::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS;
    use windows_sys::Win32::System::Pipes::PIPE_TYPE_BYTE;
    use windows_sys::Win32::System::Pipes::PIPE_WAIT;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide is NUL-terminated; the handle is returned for the
    // guard to close.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            5000,
            std::ptr::null(),
        )
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        ProbePipeGuard(None)
    } else {
        ProbePipeGuard(Some(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile() -> super::super::SandboxProfileMaterial {
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        let joined = |name: &str| format!("{}/{}", root.trim_end_matches(['/', '\\']), name);
        super::super::SandboxProfileMaterial {
            read_roots: vec![joined("s15-read-root")],
            write_roots: vec![joined("s15-write-root")],
            scratch_root: joined("s15-scratch"),
            toolchain_roots: vec![joined("s15-toolchain")],
            cache_roots: vec![joined("s15-cache")],
            git_hidden: true,
            inherited_fds: vec![0, 1, 2],
            environment_allowlist: vec!["SYSTEMROOT".into()],
            network: super::super::SandboxNetworkClass::Offline,
        }
    }

    #[test]
    fn launch_plan_maps_network_and_refuses_git_and_host_network() {
        // Offline: no capability SIDs; every grant is an explicit subtree
        // (no ancestor-traversal grants — SeChangeNotifyPrivilege covers
        // traversal, shared ancestors stay untouched).
        let plan = build_appcontainer_launch_plan(&test_profile()).expect("offline plan");
        assert!(plan.capability_sids.is_empty());
        assert!(!plan.grants.is_empty());
        assert!(plan.grants.iter().all(|grant| grant.subtree));
        assert!(plan.grants.iter().all(|grant| !grant
            .path
            .components()
            .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))));
        // PublicInternetClient: exactly the internetClient capability.
        let mut public = test_profile();
        public.network = super::super::SandboxNetworkClass::PublicInternetClient;
        let plan = build_appcontainer_launch_plan(&public).expect("public plan");
        assert!(!plan.capability_sids.is_empty());
        // HostNetwork is unsupported (P15.2).
        let mut host = test_profile();
        host.network = super::super::SandboxNetworkClass::HostNetwork;
        assert!(build_appcontainer_launch_plan(&host).is_err());
        // A .git root is never grantable (P15.1 / INV-06).
        let mut git = test_profile();
        git.read_roots = vec![format!(
            "{}/.git",
            std::env::temp_dir()
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
        )];
        let refused = build_appcontainer_launch_plan(&git);
        assert!(refused.is_err());
    }

    #[test]
    fn appcontainer_profile_create_delete_roundtrip() {
        let name = format!(
            "r-code-sandbox-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let (guard, sid) = AppContainerProfileGuard::create(&name).expect("profile");
        // AppContainer SIDs are S-1-15-3-... (authority 15, first
        // sub-authority 3); a short or wrong-authority SID is refused.
        assert!(sid.len() > 12, "sid too short: {sid:?}");
        assert_eq!(sid[0], 0x01);
        assert_eq!(sid[6], 0x00);
        assert_eq!(sid[7], 0x0f, "appcontainer authority");
        drop(guard);
        // Deletion happened: the same name can be registered again.
        let (guard, again) = AppContainerProfileGuard::create(&name).expect("recreate");
        assert_eq!(sid.len(), again.len());
        drop(guard);
    }

    #[test]
    fn internet_client_capability_derives_real_sids() {
        let sids = derive_capability_sids("internetClient").expect("derive");
        assert!(!sids.is_empty());
        for sid in &sids {
            assert_eq!(sid[0], 0x01);
            assert!(sid.len() >= 12);
        }
    }

    /// S-1-5-18 (LOCAL_SYSTEM) as raw SID bytes — a well-known valid SID
    /// the test process may grant onto files it owns.
    fn system_sid() -> Vec<u8> {
        vec![
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x12, 0x00, 0x00, 0x00,
        ]
    }

    /// The target is a DIRECTORY: subtree inheritance flags (OI|CI) are
    /// meaningful on directories, so the OS preserves them verbatim when
    /// the new DACL lands (on plain files Windows canonicalizes the flags
    /// away and the explicit-set comparison would see a mismatch).
    fn temp_target() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("marker.txt"), b"s14").expect("write marker");
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    }

    #[test]
    fn full_round_trip_prepare_apply_readback_restore() {
        let (_dir, path) = temp_target();
        let grants = vec![PlannedGrant::subtree_grant(system_sid(), 0x1FFFFF)];
        let mut record =
            build_prepared_operation("acl-roundtrip".into(), &path, &grants, 1).expect("prepared");
        let before = capture_acl(&path).expect("before capture");
        assert_eq!(record.state, AclOperationState::Prepared);
        apply_planned_grants(&path, &grants).expect("apply");
        let after = capture_acl(&path).expect("readback");
        assert_ne!(after.self_relative, record.before_descriptor);
        assert!(after
            .explicit_aces
            .iter()
            .any(|ace| ace.sid == system_sid()));
        // P14.2: the read-back classification matches Before ∪ grants.
        assert_eq!(
            classify_prepared_recovery(&before, &grants, &after),
            PreparedRecovery::AlreadyApplied
        );
        record.state = AclOperationState::Applied;
        record.actual_after = Some(after.self_relative);
        restore_before(&path, &record)
            .expect("restore executes")
            .expect("CAS holds");
        let restored = capture_acl(&path).expect("restored capture");
        assert_eq!(restored.self_relative, record.before_descriptor);
    }

    #[test]
    fn restore_refuses_an_external_edit_after_apply() {
        let (_dir, path) = temp_target();
        let grants = vec![PlannedGrant::subtree_grant(system_sid(), 0x1FFFFF)];
        let mut record =
            build_prepared_operation("acl-conflict".into(), &path, &grants, 1).expect("prepared");
        apply_planned_grants(&path, &grants).expect("apply");
        let after = capture_acl(&path).expect("readback");
        record.state = AclOperationState::Applied;
        record.actual_after = Some(after.self_relative);
        // External actor grants a DIFFERENT well-known SID (S-1-5-20,
        // NT AUTHORITY\NETWORK SERVICE).
        let foreign = vec![
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x14, 0x00, 0x00, 0x00,
        ];
        apply_planned_grants(
            &path,
            &[PlannedGrant::subtree_grant(foreign.clone(), 0x1FFFFF)],
        )
        .expect("external edit");
        let outcome = restore_before(&path, &record).expect("restore executes");
        assert_eq!(outcome, Err("acl-changed-since-apply"));
        // The external edit is preserved: the foreign SID is still there.
        let current = capture_acl(&path).expect("current");
        assert!(current.explicit_aces.iter().any(|ace| ace.sid == foreign));
    }

    #[test]
    fn prepared_recovery_classification_matrix() {
        let (_dir, path) = temp_target();
        let grants = vec![PlannedGrant::subtree_grant(system_sid(), 0x1FFFFF)];
        let before = capture_acl(&path).expect("before");
        // Unmodified target: nothing applied.
        let current = capture_acl(&path).expect("current");
        assert_eq!(
            classify_prepared_recovery(&before, &grants, &current),
            PreparedRecovery::NothingApplied
        );
        // Exactly the delta: already applied.
        apply_planned_grants(&path, &grants).expect("apply");
        let applied = capture_acl(&path).expect("applied");
        assert_eq!(
            classify_prepared_recovery(&before, &grants, &applied),
            PreparedRecovery::AlreadyApplied
        );
        // Anything else (here: BEFORE from an APPLIED state) is a
        // conflict — a state that matches neither basis.
        assert_eq!(
            classify_prepared_recovery(&applied, &grants, &before),
            PreparedRecovery::Conflict("external-change-during-prepare")
        );
        restore_to(&path, &before);
    }

    fn restore_to(path: &str, basis: &CapturedAcl) {
        let grants = vec![PlannedGrant::subtree_grant(system_sid(), 0x1FFFFF)];
        let mut record =
            build_prepared_operation("acl-cleanup".into(), path, &grants, 0).expect("prepared");
        // Direct byte-for-byte restore via the public API path: apply then
        // restore with actual_after = current.
        let current = capture_acl(path).expect("current");
        record.state = AclOperationState::Applied;
        record.actual_after = Some(current.self_relative);
        record.before_descriptor = basis.self_relative.clone();
        let _ = restore_before(path, &record);
    }
}
