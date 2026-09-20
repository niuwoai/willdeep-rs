/// 把 `"90s"`、`"2m"`、`"1h30m"`、`"1h2m3s"` 这样的时长解析成秒数。
///
/// 单位只认 `h` / `m` / `s`，必须按 h、m、s 的顺序出现，每个至多一次，
/// 每个单位前面必须有数字。空串、没有单位（`"30"`）、认不出的单位（`"1x"`）、
/// 顺序或次数不对（`"1h1h"`）都返回 `None`。
pub fn parse_duration(text: &str) -> Option<u64> {
    let _ = text;
    todo!("parse_duration is not implemented")
}
