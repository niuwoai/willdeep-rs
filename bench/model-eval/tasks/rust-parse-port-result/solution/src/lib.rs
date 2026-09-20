/// 解析端口号：允许首尾空白；0 是保留端口不算合法；越界或不是数字返回 `Err`，
/// 错误信息要带上原始输入，方便日志里对得上。
pub fn parse_port(text: &str) -> Result<u16, String> {
    let port = text
        .trim()
        .parse::<u16>()
        .map_err(|error| format!("invalid port {text:?}: {error}"))?;
    if port == 0 {
        return Err(format!("invalid port {text:?}: 0 is reserved"));
    }
    Ok(port)
}
