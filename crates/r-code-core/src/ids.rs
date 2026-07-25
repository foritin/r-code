//! ID 生成工具。

use uuid::Uuid;

/// 生成一个新的 UUID v4 字符串。
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// 生成一个带前缀的 ID（便于调试识别）。
pub fn new_prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_is_uuid_v4() {
        let id = new_id();
        let uuid = Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
    }

    #[test]
    fn new_id_is_unique() {
        let id1 = new_id();
        let id2 = new_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn prefixed_id_has_prefix() {
        let id = new_prefixed_id("task");
        assert!(id.starts_with("task_"));
    }
}
