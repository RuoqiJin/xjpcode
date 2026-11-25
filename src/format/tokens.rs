/// 估算文本字节长度对应的 Token 数量（唯一出处，按平均 3.5 字节/Token 估算）
pub fn estimate_tokens_from_bytes(byte_len: usize) -> u32 {
    ((byte_len as f64) / 3.5).ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_from_bytes() {
        assert_eq!(estimate_tokens_from_bytes(0), 0);
        assert_eq!(estimate_tokens_from_bytes(7), 2);
        assert_eq!(estimate_tokens_from_bytes(10), 3);
    }
}
