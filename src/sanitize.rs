/// Strip ANSI CSI and OSC escape sequences from untrusted text before terminal display.
pub fn sanitize_terminal_text(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            None => break,
                            Some('\x07') => break,
                            Some('\x1b') => {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
                _ => result.push(c),
            }
            continue;
        }
        result.push(c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn strips_csi_color_codes() {
        assert_eq!(
            sanitize_terminal_text("hello \x1b[31mworld\x1b[0m"),
            "hello world"
        );
    }

    #[test]
    fn strips_osc_sequences() {
        assert_eq!(
            sanitize_terminal_text("safe\x1b]0;spoofed\x07title"),
            "safetitle"
        );
    }

    #[test]
    fn preserves_plain_text() {
        assert_eq!(
            sanitize_terminal_text("Intel Driver Update 1.2"),
            "Intel Driver Update 1.2"
        );
    }
}
