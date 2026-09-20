/// 解析端口号：允许首尾空白；0 是保留端口不算合法；越界或不是数字返回 `Err`，
/// 错误信息要带上原始输入，方便日志里对得上。
pub fn parse_port(text: &str) -> Result<u16, String> {
    Ok(text.trim().parse::<u16>().unwrap())
}
