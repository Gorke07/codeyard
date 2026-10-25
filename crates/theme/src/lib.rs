//! Design tokens, generated at build time from `design/tokens*.json`.

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_generated() {
        assert_eq!(DARK.bg, 0x1c1e22);
        assert_ne!(DARK.bg, LIGHT.bg);
        assert_eq!(layout::ROW_H, 22.0);
        assert_eq!(text::CODE.0, 13.0);
        assert_eq!(SPACE[4], 8.0);
    }
}

pub mod themes;
