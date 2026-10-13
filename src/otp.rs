use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

const THRESHOLD: i32 = 4;
const MARGIN: i32 = 2;

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static APPLE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^@[\w.-]+ #([A-Za-z0-9-]{4,12})(?: %[\w.-]+)?$"));
static GOOGLE: LazyLock<Regex> = LazyLock::new(|| regex(r"\bG-([0-9]{4,8})\b"));

static KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:codes?|otp|passcodes?|passwords?|verification|verify|one-time|one time|2fa|pin|log ?in|sign-? ?in|confirm\w*|authenticat\w*|код\w*|парол\w*|подтвержд\w*|одноразов\w*|вход\w*|никому|авторизац\w*|верификац\w*)\b",
    )
});
static STRONG_GAP: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)^[\s:#.-]*(?:(?:is|was|равен|равно|будет)\s*)?[\s:-]*$")
});
static CODE_FIRST: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)^\s*(?:is|-)\s+(?:your|ваш)"));

static GROUPED: LazyLock<Regex> = LazyLock::new(|| regex(r"\b([0-9]{3,4})[ -]([0-9]{3,4})\b"));
static DIGITS: LazyLock<Regex> = LazyLock::new(|| regex(r"\b[0-9]{4,8}\b"));
static ALNUM: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(?:[A-Z0-9]{2,5}-[A-Z0-9]{2,5}|[A-Z0-9]{4,8})\b"));

const CURRENCY_AFTER: &[&str] = &["₽", "$", "€", "£", "¥", "₸", "руб", "rub", "р.", "usd", "eur", "грн", "uah"];
const CURRENCY_BEFORE: &[char] = &['₽', '$', '€', '£', '¥', '₸'];
const MONEY_WORDS: &[&str] = &["списан", "зачисл", "баланс", "balance", "amount", "сумм", "покупк", "перевод"];
const CARD_WORDS: &[&str] = &["карт", "card", "счет", "счёт", "account", "ending", "оканч"];
const ORDER_WORDS: &[&str] = &["заказ", "order", "трек", "track", "invoice", "посылк", "parcel"];
const PHONE_WORDS: &[&str] = &["call", "phone", "звон", "тел"];
const CODE_APPS: &[&str] = &["com.apple.MobileSMS", "com.apple.mobilemail", "com.google.Gmail", "ch.protonmail.protonmail"];

struct Candidate {
    value: String,
    start: usize,
    end: usize,
    alnum: bool,
}

pub fn find_code(app_id: &str, title: &str, subtitle: &str, message: &str) -> Option<String> {
    let joined = [title, subtitle, message]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let text = normalize(&joined);

    if let Some(last) = text.lines().last()
        && let Some(found) = APPLE.captures(last)
    {
        return Some(found[1].to_owned());
    }
    if let Some(found) = GOOGLE.captures(&text) {
        return Some(found[1].to_owned());
    }
    if !KEYWORD.is_match(&text) {
        return None;
    }

    let app_bonus = i32::from(CODE_APPS.contains(&app_id));
    let mut scores: HashMap<String, i32> = HashMap::new();
    for candidate in candidates(&text) {
        if let Some(score) = score(&text, &candidate) {
            let best = scores.entry(candidate.value).or_insert(i32::MIN);
            *best = (*best).max(score + app_bonus);
        }
    }

    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by_key(|(_, score)| -score);
    match ranked.as_slice() {
        [(code, best), rest @ ..]
            if *best >= THRESHOLD && rest.first().is_none_or(|(_, second)| best - second >= MARGIN) =>
        {
            Some(code.clone())
        }
        _ => None,
    }
}

fn normalize(text: &str) -> String {
    let text: String = text
        .nfkc()
        .filter(|c| !matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'))
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            c => c,
        })
        .collect();
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn candidates(text: &str) -> Vec<Candidate> {
    let mut found = Vec::new();
    for m in GROUPED.captures_iter(text) {
        let whole = m.get(0).unwrap();
        found.push(Candidate {
            value: format!("{}{}", &m[1], &m[2]),
            start: whole.start(),
            end: whole.end(),
            alnum: false,
        });
    }
    for m in DIGITS.find_iter(text) {
        found.push(Candidate { value: m.as_str().to_owned(), start: m.start(), end: m.end(), alnum: false });
    }
    for m in ALNUM.find_iter(text) {
        let value = m.as_str();
        if value.chars().any(|c| c.is_ascii_digit()) && value.chars().any(|c| c.is_ascii_uppercase()) {
            found.push(Candidate { value: value.to_owned(), start: m.start(), end: m.end(), alnum: true });
        }
    }
    found
}

fn score(text: &str, c: &Candidate) -> Option<i32> {
    let line_start = text[..c.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[c.end..].find('\n').map_or(text.len(), |i| c.end + i);
    let prefix = &text[line_start..c.start];
    let suffix = &text[c.end..line_end];
    let before: Vec<char> = prefix.chars().rev().take(2).collect();
    let after: Vec<char> = suffix.chars().take(2).collect();

    if let Some(&prev) = before.first()
        && matches!(prev, '+' | '*' | '•' | '·' | '#' | '№')
    {
        return None;
    }
    if prefix.trim_end().ends_with(CURRENCY_BEFORE) {
        return None;
    }
    // dates, times and decimals and longer digit runs.
    let digit_link = |link: char, other: Option<&char>| {
        other.is_some_and(char::is_ascii_digit) && matches!(link, '.' | ':' | '/' | ',' | ' ' | '-')
    };
    if let Some(&link) = before.first()
        && digit_link(link, before.get(1))
    {
        return None;
    }
    if let Some(&link) = after.first()
        && digit_link(link, after.get(1))
    {
        return None;
    }
    let near_after = head(suffix, 8).trim_start().to_lowercase();
    if CURRENCY_AFTER.iter().any(|unit| near_after.starts_with(unit)) || near_after == "р" {
        return None;
    }
    // inside a URL or an e-mail address.
    let token_start = prefix.rfind(char::is_whitespace).map_or(line_start, |i| line_start + i);
    let token_end = suffix.find(char::is_whitespace).map_or(line_end, |i| c.end + i);
    let token = &text[token_start..token_end];
    if token.contains(['/', '@', '=', '?', '&']) || token.contains("www.") {
        return None;
    }

    let window = tail(prefix, 40);
    let strong = KEYWORD
        .find_iter(window)
        .last()
        .is_some_and(|keyword| STRONG_GAP.is_match(&window[keyword.end()..]))
        || CODE_FIRST.is_match(suffix);
    if c.alnum && !strong {
        return None;
    }

    let mut score = 1;
    if KEYWORD.is_match(&text[line_start..line_end]) {
        score += 3;
    }
    if strong {
        score += 4;
    }
    if c.value.len() == 6 {
        score += 1;
    }
    if prefix.trim().is_empty() && suffix.trim().is_empty() {
        score += 1;
    }

    let near_before = tail(prefix, 20).to_lowercase();
    if !strong && c.value.len() == 4 && matches!(c.value.parse::<u32>(), Ok(1900..=2099)) {
        score -= 3;
    }
    if MONEY_WORDS.iter().any(|w| near_before.contains(w)) {
        score -= 4;
    }
    if CARD_WORDS.iter().any(|w| near_before.contains(w)) {
        score -= 6;
    }
    if ORDER_WORDS.iter().any(|w| tail(prefix, 15).to_lowercase().contains(w)) {
        score -= 6;
    }
    if PHONE_WORDS.iter().any(|w| near_before.contains(w)) {
        score -= 6;
    }
    Some(score)
}

fn tail(s: &str, n: usize) -> &str {
    s.char_indices().rev().nth(n.saturating_sub(1)).map_or(s, |(i, _)| &s[i..])
}

fn head(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(message: &str) -> Option<String> {
        find_code("com.example", "", "", message)
    }

    #[test]
    fn finds_codes() {
        let cases = [
            ("Код: 4821. Никому не сообщайте", "4821"),
            ("Код подтверждения для входа: 123456", "123456"),
            ("G-482913 is your Google verification code.", "482913"),
            ("Use 7781 to verify your Microsoft account", "7781"),
            ("Your verification code is 123 456", "123456"),
            ("482913 — ваш код для входа в Яндекс ID", "482913"),
            ("Код 1234 для подтверждения операции на 1500 ₽ по карте *4821", "1234"),
            ("Your code: K7X-9QP", "K7X-9QP"),
            ("Your Example code is 123456.\n\n@example.com #123456", "123456"),
            ("Никому не сообщайте код 5521! Вход в СберБанк Онлайн", "5521"),
            ("Ваш PIN: 0042", "0042"),
            ("Your code is 2025", "2025"),
        ];
        for (message, expected) in cases {
            assert_eq!(code(message).as_deref(), Some(expected), "{message}");
        }
    }

    #[test]
    fn sees_through_obfuscation() {
        assert_eq!(code("Code: １２３４５６").as_deref(), Some("123456"));
        assert_eq!(code("Код: 12\u{200B}34").as_deref(), Some("1234"));
        assert_eq!(code("Your code is 123\u{A0}456").as_deref(), Some("123456"));
    }

    #[test]
    fn finds_codes_in_the_title() {
        assert_eq!(find_code("com.example", "Код: 4821", "", "Никому не сообщайте").as_deref(), Some("4821"));
    }

    #[test]
    fn ignores_non_codes() {
        let cases = [
            "Ваш заказ №123456 доставлен",
            "Заказ №123456 подтверждён",
            "Your code is 1234\nYour code is 5678",
            "Meeting moved to room 4012",
        ];
        for message in cases {
            assert_eq!(code(message), None, "{message}");
        }
    }
}
