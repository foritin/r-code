//! 粘贴折叠（M4-02 / R-EDIT-02）。
//!
//! bracketed paste 超过阈值（>1000 字符）折叠为编号占位符进编辑器；
//! 原文存 PasteBuffer，发送时展开（`expand_pasted`）——上下文拿到的是
//! 完整原文，编辑器里只看到一行占位（codex `[Pasted Content #N]` 形态）。

pub const PASTE_THRESHOLD: usize = 1000;

/// 一次折叠粘贴的原文（仅内存持有；会话生命周期）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteBlob {
    pub id: u32,
    pub chars: usize,
    pub content: String,
}

/// 折叠粘贴登记簿。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PasteBuffer {
    next_id: u32,
    blobs: Vec<PasteBlob>,
}

/// 占位符（codex 形态：`[Pasted Content #N +M chars]`）。
pub fn paste_placeholder(id: u32, chars: usize) -> String {
    format!("[Pasted Content #{id} +{chars} chars]")
}

impl PasteBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一段大粘贴，返回占位符文本。
    pub fn register(&mut self, content: String) -> String {
        self.next_id += 1;
        let chars = content.chars().count();
        self.blobs.push(PasteBlob {
            id: self.next_id,
            chars,
            content,
        });
        paste_placeholder(self.next_id, chars)
    }

    /// 展开占位符为原文（发送路径用；未登记的占位符原样保留）。
    pub fn expand(&self, text: &str) -> String {
        let mut expanded = text.to_string();
        for blob in &self.blobs {
            let placeholder = paste_placeholder(blob.id, blob.chars);
            expanded = expanded.replace(&placeholder, &blob.content);
        }
        expanded
    }
}

/// 折叠判定（>1000 字符；含换行的大粘贴）。
pub fn should_fold(text: &str) -> bool {
    text.chars().count() > PASTE_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(n: usize) -> String {
        "x".repeat(n)
    }

    /// M4-02.A1：>1000 字符折叠为编号占位符；编号递增；小粘贴不折叠。
    #[test]
    fn large_pastes_fold_into_numbered_placeholders() {
        assert!(!should_fold(&big(1000)), "恰好 1000 不折叠（> 阈值）");
        assert!(should_fold(&big(1001)));

        let mut pastes = PasteBuffer::new();
        let first = pastes.register(big(1200));
        assert_eq!(first, "[Pasted Content #1 +1200 chars]");
        let second = pastes.register(format!("{}\n{}", big(50), big(60)));
        assert_eq!(second, "[Pasted Content #2 +111 chars]");
        // 编号稳定递增。
        assert_eq!(
            pastes.register(big(2000)),
            "[Pasted Content #3 +2000 chars]"
        );
    }

    /// M4-02.A3：发送内容含折叠原文（占位符展开，未登记的占位符保留原样）。
    #[test]
    fn expansion_restores_original_content_on_send() {
        let mut pastes = PasteBuffer::new();
        let placeholder = pastes.register("第一段原文\n多行内容".to_string());
        let draft = format!("开头 {placeholder} 结尾");
        let expanded = pastes.expand(&draft);
        assert!(
            expanded.contains("第一段原文\n多行内容"),
            "发送内容必须含原文：{expanded}"
        );
        assert!(!expanded.contains("[Pasted Content"), "占位符必须全部展开");
        // 未登记的占位符（如手打的）不被误替换。
        let untouched = pastes.expand("手打 [Pasted Content #99 +1 chars]");
        assert!(untouched.contains("[Pasted Content #99"), "{untouched}");
    }
}
