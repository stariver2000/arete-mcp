//! 파이썬 글 함수의 흉내 (도구 몸통이 쓰는 것만): str.isspace·str.split()·casefold·re `\s`.

/// 파이썬 str.isspace (= str.split()·re `\s` 의 공백). Rust 의 White_Space 에 더해 \x1c–\x1f 를 공백으로 본다.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// 파이썬 `" ".join(text.split())`: 공백 덩어리를 한 칸으로, 앞뒤 공백을 뺀다.
pub fn squeeze_spaces(text: &str) -> String {
    text.split(is_py_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 파이썬 casefold 의 근사: 소문자 + 독일어 ß·그리스어 끝 시그마 (분야 이름 맞추기에만 쓴다, 분야 이름은 한국어·영어).
pub fn casefold(text: &str) -> String {
    text.to_lowercase().replace('ß', "ss").replace('ς', "σ")
}

/// 파이썬 server._squash: `re.sub(r"[\s·/_.-]+", "", text.casefold())`
pub fn squash(text: &str) -> String {
    casefold(text)
        .chars()
        .filter(|c| !(is_py_space(*c) || matches!(c, '·' | '/' | '_' | '.' | '-')))
        .collect()
}

/// 파이썬 `text[:n]` (글자 수)
pub fn take_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squeeze_like_python_split() {
        assert_eq!(squeeze_spaces("  회의록 \u{3000}요약\x1c\n x "), "회의록 요약 x");
        assert_eq!(squeeze_spaces("   "), "");
    }

    #[test]
    fn squash_like_python() {
        assert_eq!(squash("Video · Short_Form/x-y.z"), "videoshortformxyz");
    }
}
