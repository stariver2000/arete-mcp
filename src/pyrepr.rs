//! 파이썬 repr 흉내 (pydantic 검사 글의 `input_value=…` 가 파이썬 객체의 repr 이라서).
//!
//! 입력은 JSON 값이다: 파이썬 쪽은 같은 JSON 을 json.loads 한 객체(dict·list·str·int·float·bool·None)의 repr 을 쓴다.
//! 숫자는 serde_json `arbitrary_precision` 으로 원문 글을 받아 파이썬처럼 int/float 를 가른다 ('.'·e·E 가 있으면 float).

use serde_json::{Number, Value};

/// JSON 숫자 원문이 파이썬 json.loads 에서 float 가 되나 (그 밖은 int).
pub fn is_float_text(text: &str) -> bool {
    text.contains(['.', 'e', 'E'])
}

/// 파이썬 `type(x).__name__` (pydantic 의 `input_type=`).
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if is_float_text(&n.to_string()) => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

pub fn repr(value: &Value) -> String {
    let mut out = String::new();
    write_repr(value, &mut out);
    out
}

fn write_repr(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::String(s) => out.push_str(&str_repr(s)),
        Value::Array(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                write_repr(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (n, (key, item)) in map.iter().enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                out.push_str(&str_repr(key));
                out.push_str(": ");
                write_repr(item, out);
            }
            out.push('}');
        }
    }
}

fn number_repr(n: &Number) -> String {
    let text = n.to_string();
    if !is_float_text(&text) {
        return int_text(&text);
    }
    float_repr(text.parse::<f64>().unwrap_or(f64::NAN))
}

/// JSON 정수 원문 → 파이썬 int 의 str (`-0` → `0`, 앞 0 은 JSON 이 막는다).
pub fn int_text(text: &str) -> String {
    if text.trim_start_matches('-').chars().all(|c| c == '0') {
        "0".to_string()
    } else {
        text.to_string()
    }
}

/// 파이썬 float repr: 왕복되는 가장 짧은 숫자, 지수가 -4 이상 16 미만이면 소수점 꼴 (`1e+16`·`1e-05`·`1000000000000000.0`).
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let sci = format!("{value:e}"); // 예: "1.5e300", "-5e0", "0e0"
    let (mantissa, exp) = sci.split_once('e').expect("Rust {:e} 꼴");
    let exp: i32 = exp.parse().expect("지수");
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let point = exp as usize + 1;
            let (whole, frac) = if digits.len() > point {
                (digits[..point].to_string(), digits[point..].to_string())
            } else {
                (format!("{digits:0<point$}"), String::new())
            };
            let frac = if frac.is_empty() { "0".to_string() } else { frac };
            format!("{sign}{whole}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let rest = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let exp_sign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{}{rest}e{exp_sign}{:02}", &digits[..1], exp.abs())
    }
}

/// 파이썬 str repr: 작은따옴표 (글에 ' 만 있고 " 가 없으면 큰따옴표), 인쇄할 수 없는 글자는 \x·\u·\U.
pub fn str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let code = c as u32;
                if code < 0x100 {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code < 0x10000 {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// 파이썬 str.isprintable 의 근사: 제어(Cc)·서식(Cf)·구분(Zl·Zp)·공백(Zs, ' ' 빼고)·사용자 영역(Co)·대리(Cs).
/// 유니코드 표 전체를 넣지 않는다 — 배정 안 된 글자(Cn)는 인쇄 가능으로 본다 (알려진 한계).
fn printable(c: char) -> bool {
    let code = c as u32;
    !matches!(code,
        0x00..=0x1f | 0x7f..=0xa0 | 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2
        | 0x1680 | 0x180e | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f
        | 0x3000 | 0xd800..=0xf8ff | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f
        | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f | 0xf0000..=0x10ffff)
}

/// pydantic-core 의 input_value 자르기: repr 이 50바이트를 넘으면 앞 25바이트 … 뒤 24바이트 (글자 경계로 맞춤).
/// 근거: 한글 17자 repr(53바이트)이 앞 8자·뒤 7자로 잘린다 (arete-mcp 0.4.4·pydantic 2.14 에서 확인, 2026-11-27).
pub fn truncated(repr: &str) -> String {
    if repr.len() <= 50 {
        return repr.to_string();
    }
    let mut start = 25;
    while !repr.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = repr.len() - 24;
    while !repr.is_char_boundary(end) {
        end += 1;
    }
    format!("{}...{}", &repr[..start], &repr[end..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_like_python() {
        for (value, want) in [
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (123456.789, "123456.789"),
            (-5.0, "-5.0"),
            (2.5, "2.5"),
            (1.5e300, "1.5e+300"),
            (9.223372036854776e18, "9.223372036854776e+18"),
            (-0.0, "-0.0"),
        ] {
            assert_eq!(float_repr(value), want);
        }
    }

    #[test]
    fn strings_and_containers_like_python() {
        assert_eq!(str_repr("it's \"q\""), r#"'it\'s "q"'"#);
        assert_eq!(str_repr("ab'c"), r#""ab'c""#);
        assert_eq!(str_repr("\u{a0}\u{3000}\u{200b}\0"), r"'\xa0\u3000\u200b\x00'");
        assert_eq!(
            repr(&json!({"limit": "a", "ko": true, "kind": 3, "x": null, "y": [1.0]})),
            "{'limit': 'a', 'ko': True, 'kind': 3, 'x': None, 'y': [1.0]}"
        );
    }

    #[test]
    fn truncation_on_byte_length() {
        assert_eq!(truncated(&str_repr(&"가".repeat(16))), str_repr(&"가".repeat(16)));
        assert_eq!(truncated(&str_repr(&"가".repeat(17))), "'가가가가가가가가...가가가가가가가'");
        assert_eq!(
            truncated(&str_repr(&"a".repeat(49))),
            format!("'{}...{}'", "a".repeat(24), "a".repeat(23))
        );
    }
}
