use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use r_code_core::dto::{AgentEventScope, SubagentState};
use serde::Serialize;

pub(crate) const MAX_PEER_MESSAGE_CHARS: usize = 4_000;
pub(crate) const MAX_PEER_MESSAGE_ID_CHARS: usize = 128;
pub(crate) const MAX_PEER_MAILBOX_MESSAGES: usize = 32;
pub(crate) const MAX_PEER_MAILBOX_CHARS: usize = 32_000;
const MAX_PEER_MESSAGE_IDS_PER_TREE: usize = 1_024;

#[derive(Debug, Clone)]
pub(crate) struct QueuedPeerMessage {
    pub(crate) message_id: String,
    pub(crate) sender_agent_id: String,
    pub(crate) recipient_agent_id: String,
    pub(crate) content: String,
    pub(crate) content_chars: usize,
    pub(crate) sender_scope: AgentEventScope,
}

#[derive(Debug, Clone)]
pub(crate) enum SendPeerMessageOutcome {
    // QueuedPeerMessage 携带完整的 AgentEventScope，比 Duplicate 变体大一个数量级；
    // 装箱避免每次匹配复制整个枚举。
    Queued(Box<QueuedPeerMessage>),
    Duplicate {
        message_id: String,
        sender_agent_id: String,
        recipient_agent_id: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum TerminalClaim {
    Claimed,
    PendingMessages(Vec<QueuedPeerMessage>),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PeerAgentListing {
    pub(crate) agent_id: String,
    pub(crate) parent_agent_id: Option<String>,
    pub(crate) depth: u8,
    pub(crate) label: Option<String>,
    pub(crate) runtime_kind: String,
    pub(crate) model: Option<String>,
    pub(crate) access: String,
    pub(crate) require_approval: bool,
    pub(crate) state: String,
    pub(crate) relationship: String,
    pub(crate) can_message: bool,
}

struct DelegationNode {
    scope: AgentEventScope,
    depth: u8,
    state: SubagentState,
    accepts_peer_messages: bool,
    mailbox: VecDeque<QueuedPeerMessage>,
}

impl DelegationNode {
    fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            SubagentState::Completed | SubagentState::Failed | SubagentState::Cancelled
        )
    }
}

struct DelegationTreeInner {
    nodes: HashMap<String, DelegationNode>,
    seen_message_ids: HashSet<(String, String)>,
    queued_messages: usize,
    queued_chars: usize,
}

/// One root run's bounded topology and in-memory peer mailboxes.
///
/// The tree is deliberately process-local: peer content is neither canonical model history nor a
/// persisted transcript. All mutations are short, non-async critical sections so caller identity,
/// relationship checks, deduplication and capacity accounting happen atomically.
pub(crate) struct DelegationTree {
    root_run_id: String,
    inner: Mutex<DelegationTreeInner>,
}

impl DelegationTree {
    pub(crate) fn new(root_scope: AgentEventScope) -> Self {
        let root_run_id = root_scope.run_id.clone();
        let mut nodes = HashMap::new();
        nodes.insert(
            root_run_id.clone(),
            DelegationNode {
                scope: root_scope,
                depth: 0,
                state: SubagentState::Running,
                accepts_peer_messages: true,
                mailbox: VecDeque::new(),
            },
        );
        Self {
            root_run_id,
            inner: Mutex::new(DelegationTreeInner {
                nodes,
                seen_message_ids: HashSet::new(),
                queued_messages: 0,
                queued_chars: 0,
            }),
        }
    }

    pub(crate) fn root_run_id(&self) -> &str {
        &self.root_run_id
    }

    pub(crate) fn register_child(
        &self,
        scope: AgentEventScope,
        accepts_peer_messages: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("delegation tree lock poisoned");
        let parent_id = scope
            .parent_run_id
            .as_deref()
            .ok_or_else(|| "子代理节点缺少直接父运行 ID".to_string())?;
        let parent = inner
            .nodes
            .get(parent_id)
            .ok_or_else(|| "子代理父节点不在当前运行树中".to_string())?;
        if parent.is_terminal() {
            return Err("子代理父节点已经结束".to_string());
        }
        let child_depth = parent.depth.saturating_add(1);
        if inner.nodes.contains_key(&scope.run_id) {
            return Err(format!("重复的 Agent 运行 ID：{}", scope.run_id));
        }
        inner.nodes.insert(
            scope.run_id.clone(),
            DelegationNode {
                scope,
                depth: child_depth,
                state: SubagentState::Queued,
                accepts_peer_messages,
                mailbox: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub(crate) fn mark_running(&self, agent_id: &str) {
        self.set_state(agent_id, SubagentState::Running);
    }

    pub(crate) fn mark_terminal(&self, agent_id: &str, state: SubagentState) {
        debug_assert!(matches!(
            state,
            SubagentState::Completed | SubagentState::Failed | SubagentState::Cancelled
        ));
        self.set_state(agent_id, state);
    }

    /// Atomically close a normally completing recipient or hand its newly queued mail back to the
    /// caller for one more Provider turn. A concurrent sender therefore observes exactly one side
    /// of the same lock: either its message is accepted and drained here, or terminal state is
    /// claimed first and the send fails.
    pub(crate) fn claim_terminal_or_drain(
        &self,
        agent_id: &str,
        state: SubagentState,
    ) -> Result<TerminalClaim, String> {
        debug_assert!(matches!(
            state,
            SubagentState::Completed | SubagentState::Failed | SubagentState::Cancelled
        ));
        let mut inner = self.inner.lock().expect("delegation tree lock poisoned");
        let messages = drain_mailbox(&mut inner, agent_id)?;
        if messages.is_empty() {
            inner
                .nodes
                .get_mut(agent_id)
                .expect("node checked by drain_mailbox")
                .state = state;
            Ok(TerminalClaim::Claimed)
        } else {
            Ok(TerminalClaim::PendingMessages(messages))
        }
    }

    fn set_state(&self, agent_id: &str, state: SubagentState) {
        if let Some(node) = self
            .inner
            .lock()
            .expect("delegation tree lock poisoned")
            .nodes
            .get_mut(agent_id)
        {
            node.state = state;
        }
    }

    pub(crate) fn list_visible_agents(
        &self,
        caller_agent_id: &str,
    ) -> Result<Vec<PeerAgentListing>, String> {
        let inner = self.inner.lock().expect("delegation tree lock poisoned");
        let caller = inner
            .nodes
            .get(caller_agent_id)
            .ok_or_else(|| "当前 Agent 不在活动运行树中".to_string())?;
        let caller_active = !caller.is_terminal() && caller.accepts_peer_messages;
        let mut agents = inner
            .nodes
            .values()
            .map(|node| {
                let relationship = relationship(caller, node).unwrap_or("unrelated");
                let can_message = matches!(relationship, "parent" | "child" | "sibling")
                    && caller_active
                    && !node.is_terminal()
                    && node.accepts_peer_messages;
                PeerAgentListing {
                    agent_id: node.scope.agent_id.clone(),
                    parent_agent_id: node.scope.parent_run_id.clone(),
                    depth: node.depth,
                    label: node.scope.agent_label.clone(),
                    runtime_kind: node.scope.runtime_kind.to_string(),
                    model: node.scope.model.clone(),
                    access: node.scope.access_mode.to_string(),
                    require_approval: node.scope.require_approval,
                    state: node.state.to_string(),
                    relationship: relationship.to_string(),
                    can_message,
                }
            })
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(agents)
    }

    pub(crate) fn send(
        &self,
        sender_agent_id: &str,
        recipient_agent_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<SendPeerMessageOutcome, String> {
        validate_message_id(message_id)?;
        let content_chars = content.chars().count();
        if content.trim().is_empty() {
            return Err("send_agent_message requires non-empty content".to_string());
        }
        if content
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(
                "peer message contains a disallowed control character (including NUL)".to_string(),
            );
        }
        if content_chars > MAX_PEER_MESSAGE_CHARS {
            return Err(format!(
                "peer message exceeds the {MAX_PEER_MESSAGE_CHARS}-character limit"
            ));
        }

        let mut inner = self.inner.lock().expect("delegation tree lock poisoned");
        let sender = inner
            .nodes
            .get(sender_agent_id)
            .ok_or_else(|| "发送方不在当前运行树中".to_string())?;
        if sender.is_terminal() || !sender.accepts_peer_messages {
            return Err("发送方不支持活动的 Agent 通信".to_string());
        }
        let recipient = inner
            .nodes
            .get(recipient_agent_id)
            .ok_or_else(|| "目标 Agent 不在当前运行树中（拒绝跨树通信）".to_string())?;
        if sender_agent_id == recipient_agent_id || relationship(sender, recipient).is_none() {
            return Err("只允许向直接父、直接子或同一直接父下的兄弟 Agent 发送消息".to_string());
        }

        let dedupe_key = (sender_agent_id.to_string(), message_id.to_string());
        if inner.seen_message_ids.contains(&dedupe_key) {
            return Ok(SendPeerMessageOutcome::Duplicate {
                message_id: message_id.to_string(),
                sender_agent_id: sender_agent_id.to_string(),
                recipient_agent_id: recipient_agent_id.to_string(),
            });
        }
        if recipient.is_terminal() {
            return Err("目标 Agent 已经结束，不能再接收消息".to_string());
        }
        if !recipient.accepts_peer_messages {
            return Err("目标 Agent 是不支持实时通信的外部或适配器叶节点".to_string());
        }
        if inner.seen_message_ids.len() >= MAX_PEER_MESSAGE_IDS_PER_TREE {
            return Err("Agent 消息幂等键预算已满，请结束当前运行树后重试".to_string());
        }
        if inner.queued_messages >= MAX_PEER_MAILBOX_MESSAGES
            || inner.queued_chars.saturating_add(content_chars) > MAX_PEER_MAILBOX_CHARS
        {
            return Err("目标 mailbox 正在背压：请等待对方处理已有消息后重试".to_string());
        }

        let queued = QueuedPeerMessage {
            message_id: message_id.to_string(),
            sender_agent_id: sender_agent_id.to_string(),
            recipient_agent_id: recipient_agent_id.to_string(),
            content: content.to_string(),
            content_chars,
            sender_scope: sender.scope.clone(),
        };
        inner.seen_message_ids.insert(dedupe_key);
        inner.queued_messages += 1;
        inner.queued_chars += content_chars;
        inner
            .nodes
            .get_mut(recipient_agent_id)
            .expect("recipient checked above")
            .mailbox
            .push_back(queued.clone());
        Ok(SendPeerMessageOutcome::Queued(Box::new(queued)))
    }

    pub(crate) fn drain(&self, recipient_agent_id: &str) -> Result<Vec<QueuedPeerMessage>, String> {
        let mut inner = self.inner.lock().expect("delegation tree lock poisoned");
        drain_mailbox(&mut inner, recipient_agent_id)
    }
}

fn drain_mailbox(
    inner: &mut DelegationTreeInner,
    recipient_agent_id: &str,
) -> Result<Vec<QueuedPeerMessage>, String> {
    let messages = {
        let recipient = inner
            .nodes
            .get_mut(recipient_agent_id)
            .ok_or_else(|| "目标 Agent 不在当前运行树中".to_string())?;
        recipient.mailbox.drain(..).collect::<Vec<_>>()
    };
    let drained_chars = messages
        .iter()
        .map(|message| message.content_chars)
        .sum::<usize>();
    inner.queued_messages = inner.queued_messages.saturating_sub(messages.len());
    inner.queued_chars = inner.queued_chars.saturating_sub(drained_chars);
    Ok(messages)
}

fn relationship<'a>(caller: &'a DelegationNode, other: &'a DelegationNode) -> Option<&'static str> {
    if caller.scope.agent_id == other.scope.agent_id {
        return Some("self");
    }
    if caller.scope.parent_run_id.as_deref() == Some(other.scope.agent_id.as_str()) {
        return Some("parent");
    }
    if other.scope.parent_run_id.as_deref() == Some(caller.scope.agent_id.as_str()) {
        return Some("child");
    }
    if caller.scope.parent_run_id.is_some()
        && caller.scope.parent_run_id == other.scope.parent_run_id
    {
        return Some("sibling");
    }
    None
}

fn validate_message_id(message_id: &str) -> Result<(), String> {
    if message_id.trim().is_empty() || message_id.trim() != message_id {
        return Err("message_id must be non-empty and have no surrounding whitespace".to_string());
    }
    if message_id.chars().count() > MAX_PEER_MESSAGE_ID_CHARS {
        return Err(format!(
            "message_id exceeds the {MAX_PEER_MESSAGE_ID_CHARS}-character limit"
        ));
    }
    if message_id.chars().any(char::is_control) {
        return Err("message_id contains a disallowed control character".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use r_code_core::dto::{AgentKind, AgentRunRuntimeKind, SubagentAccessMode, SubagentState};

    use super::*;

    fn scope(agent_id: &str, parent: Option<&str>, native: bool) -> AgentEventScope {
        AgentEventScope {
            run_id: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            parent_run_id: parent.map(ToOwned::to_owned),
            agent_kind: if parent.is_some() {
                AgentKind::Subagent
            } else {
                AgentKind::Main
            },
            agent_label: Some(agent_id.to_string()),
            delegated_by_tool_call_id: None,
            runtime_kind: if native {
                AgentRunRuntimeKind::Native
            } else {
                AgentRunRuntimeKind::CodexExec
            },
            model: Some("test-model".to_string()),
            access_mode: SubagentAccessMode::ReadOnly,
            require_approval: false,
            routing_reason: None,
        }
    }

    fn fixture() -> DelegationTree {
        let tree = DelegationTree::new(scope("root", None, true));
        tree.register_child(scope("child-a", Some("root"), true), true)
            .unwrap();
        tree.register_child(scope("child-b", Some("root"), true), true)
            .unwrap();
        tree.register_child(scope("grand-a", Some("child-a"), true), true)
            .unwrap();
        tree.register_child(scope("grand-b", Some("child-b"), true), true)
            .unwrap();
        tree
    }

    #[test]
    fn only_direct_parent_child_and_siblings_can_exchange_messages() {
        let tree = fixture();
        for (sender, recipient) in [
            ("root", "child-a"),
            ("child-a", "root"),
            ("child-a", "child-b"),
            ("child-a", "grand-a"),
            ("grand-a", "child-a"),
        ] {
            let id = format!("{sender}-to-{recipient}");
            assert!(tree.send(sender, recipient, &id, "bounded update").is_ok());
        }

        for (sender, recipient) in [
            ("root", "grand-a"),
            ("grand-a", "root"),
            ("grand-a", "child-b"),
            ("grand-a", "grand-b"),
            ("child-a", "child-a"),
        ] {
            let error = tree
                .send(
                    sender,
                    recipient,
                    &format!("denied-{sender}-{recipient}"),
                    "no",
                )
                .unwrap_err();
            assert!(
                error.contains("直接父") || error.contains("直接子"),
                "{error}"
            );
        }
        assert!(tree
            .send("child-a", "other-tree", "cross-tree", "no")
            .unwrap_err()
            .contains("跨树"));
    }

    #[test]
    fn duplicate_ids_do_not_enqueue_twice_and_remain_stable_after_delivery() {
        let tree = fixture();
        assert!(matches!(
            tree.send("root", "child-a", "stable-1", "first").unwrap(),
            SendPeerMessageOutcome::Queued(_)
        ));
        assert!(matches!(
            tree.send("root", "child-a", "stable-1", "changed retry")
                .unwrap(),
            SendPeerMessageOutcome::Duplicate { .. }
        ));
        let delivered = tree.drain("child-a").unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].content, "first");
        assert!(matches!(
            tree.send("root", "child-a", "stable-1", "later retry")
                .unwrap(),
            SendPeerMessageOutcome::Duplicate { .. }
        ));
        assert!(tree.drain("child-a").unwrap().is_empty());
    }

    #[test]
    fn mailbox_applies_message_count_and_character_limits() {
        let tree = fixture();
        for index in 0..MAX_PEER_MAILBOX_MESSAGES {
            tree.send("root", "child-a", &format!("m-{index}"), "x")
                .unwrap();
        }
        assert!(tree
            .send("root", "child-a", "overflow", "x")
            .unwrap_err()
            .contains("背压"));
        tree.drain("child-a").unwrap();
        assert!(tree
            .send(
                "root",
                "child-a",
                "too-long",
                &"x".repeat(MAX_PEER_MESSAGE_CHARS + 1),
            )
            .unwrap_err()
            .contains("character limit"));
        assert!(tree
            .send("root", "child-a", "nul-content", "bad\0payload")
            .unwrap_err()
            .contains("control character"));
        assert!(tree
            .send("root", "child-a", "bad\nmessage-id", "content")
            .unwrap_err()
            .contains("control character"));
        assert!(tree
            .send("root", "child-a", "multiline-ok", "line 1\nline 2\tvalue")
            .is_ok());
    }

    #[test]
    fn terminal_and_external_leaf_recipients_fail_closed() {
        let tree = fixture();
        tree.register_child(scope("codex", Some("root"), false), false)
            .unwrap();
        assert!(tree
            .send("root", "codex", "leaf", "no")
            .unwrap_err()
            .contains("叶节点"));
        tree.mark_terminal("child-a", SubagentState::Completed);
        assert!(tree
            .send("root", "child-a", "terminal", "no")
            .unwrap_err()
            .contains("已经结束"));
    }

    #[test]
    fn list_agents_reveals_the_tree_but_only_marks_topology_neighbors_messageable() {
        let tree = fixture();
        let visible = tree.list_visible_agents("grand-a").unwrap();
        let ids = visible
            .iter()
            .map(|entry| entry.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["child-a", "child-b", "grand-a", "grand-b", "root"]
        );
        assert_eq!(
            visible
                .iter()
                .find(|entry| entry.agent_id == "child-a")
                .unwrap()
                .relationship,
            "parent"
        );
        assert!(
            visible
                .iter()
                .find(|entry| entry.agent_id == "child-a")
                .unwrap()
                .can_message
        );
        let root = visible
            .iter()
            .find(|entry| entry.agent_id == "root")
            .unwrap();
        assert_eq!(root.depth, 0);
        assert_eq!(root.relationship, "unrelated");
        assert!(!root.can_message);
        assert_eq!(
            visible
                .iter()
                .find(|entry| entry.agent_id == "grand-a")
                .unwrap()
                .depth,
            2
        );
    }

    #[test]
    fn terminal_claim_race_either_delivers_the_message_or_rejects_the_send() {
        use std::sync::{Arc, Barrier};

        for index in 0..100 {
            let tree = Arc::new(fixture());
            let barrier = Arc::new(Barrier::new(3));
            let send_tree = tree.clone();
            let send_barrier = barrier.clone();
            let sender = std::thread::spawn(move || {
                send_barrier.wait();
                send_tree.send(
                    "root",
                    "child-a",
                    &format!("race-{index}"),
                    "must not be stranded",
                )
            });
            let claim_tree = tree.clone();
            let claim_barrier = barrier.clone();
            let claimant = std::thread::spawn(move || {
                claim_barrier.wait();
                claim_tree.claim_terminal_or_drain("child-a", SubagentState::Completed)
            });
            barrier.wait();

            match (sender.join().unwrap(), claimant.join().unwrap().unwrap()) {
                (
                    Ok(SendPeerMessageOutcome::Queued(_)),
                    TerminalClaim::PendingMessages(messages),
                ) => {
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0].content, "must not be stranded");
                }
                (Err(error), TerminalClaim::Claimed) => {
                    assert!(error.contains("已经结束"), "{error}");
                }
                (send, claim) => panic!("invalid race outcome: send={send:?}, claim={claim:?}"),
            }
        }
    }
}
