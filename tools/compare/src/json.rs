use std::fmt::{Display, Write};

/// Serializes a value into the project's compact, deterministic JSON form.
///
/// Implementations emit object keys in a fixed lexical order, preserve the
/// semantic order of arrays, and do not emit insignificant whitespace.
pub trait CanonicalJson {
    /// Appends canonical JSON to `output`.
    fn write_canonical_json(&self, output: &mut String);

    /// Returns the canonical JSON representation.
    fn to_canonical_json(&self) -> String {
        let mut output = String::new();
        self.write_canonical_json(&mut output);
        output
    }
}

/// Quotes and escapes a UTF-8 string as one canonical JSON string value.
pub fn canonical_json_string(input: &str) -> String {
    let mut output = String::with_capacity(input.len().saturating_add(2));
    push_json_string(&mut output, input);
    output
}

pub(crate) fn push_json_string(output: &mut String, input: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    output.push('"');
    for character in input.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if control <= '\u{001f}' => {
                let value = control as usize;
                output.push_str("\\u00");
                output.push(char::from(HEX[(value >> 4) & 0x0f]));
                output.push(char::from(HEX[value & 0x0f]));
            }
            value => output.push(value),
        }
    }
    output.push('"');
}

pub(crate) fn push_number(output: &mut String, value: impl Display) {
    write!(output, "{value}").expect("writing formatted values to String is infallible");
}

pub(crate) fn push_optional_u64(output: &mut String, value: Option<u64>) {
    if let Some(value) = value {
        push_number(output, value);
    } else {
        output.push_str("null");
    }
}

pub(crate) fn push_optional_u32(output: &mut String, value: Option<u32>) {
    if let Some(value) = value {
        push_number(output, value);
    } else {
        output.push_str("null");
    }
}

pub(crate) fn push_optional_usize(output: &mut String, value: Option<usize>) {
    if let Some(value) = value {
        push_number(output, value);
    } else {
        output.push_str("null");
    }
}

pub(crate) fn push_u32_array(output: &mut String, values: &[u32]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        push_number(output, value);
    }
    output.push(']');
}

pub(crate) fn push_i64_array<const N: usize>(output: &mut String, values: &[i64; N]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        push_number(output, value);
    }
    output.push(']');
}

pub(crate) fn push_u8_array<const N: usize>(output: &mut String, values: &[u8; N]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        push_number(output, value);
    }
    output.push(']');
}

pub(crate) fn push_hex(output: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    output.push('"');
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::canonical_json_string;

    #[test]
    fn escapes_json_syntax_and_every_control_form() {
        let input = "\"\\\u{0008}\u{000c}\n\r\t\u{0000}\u{001f}é😀";
        assert_eq!(
            canonical_json_string(input),
            "\"\\\"\\\\\\b\\f\\n\\r\\t\\u0000\\u001fé😀\""
        );
    }

    #[test]
    fn leaves_valid_non_ascii_utf8_unescaped() {
        assert_eq!(canonical_json_string("PDF 文本 🦀"), "\"PDF 文本 🦀\"");
    }
}
