/// 把 `"90s"`、`"2m"`、`"1h30m"`、`"1h2m3s"` 这样的时长解析成秒数。
///
/// 单位只认 `h` / `m` / `s`，必须按 h、m、s 的顺序出现，每个至多一次，
/// 每个单位前面必须有数字。空串、没有单位（`"30"`）、认不出的单位（`"1x"`）、
/// 顺序或次数不对（`"1h1h"`）都返回 `None`。
pub fn parse_duration(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    let mut last_rank = 0;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let (rank, factor) = match ch {
            'h' => (1, 3600),
            'm' => (2, 60),
            's' => (3, 1),
            _ => return None,
        };
        if number.is_empty() || rank <= last_rank {
            return None;
        }
        let value: u64 = number.parse().ok()?;
        total = total.checked_add(value.checked_mul(factor)?)?;
        number.clear();
        last_rank = rank;
    }
    if !number.is_empty() {
        return None;
    }
    Some(total)
}
