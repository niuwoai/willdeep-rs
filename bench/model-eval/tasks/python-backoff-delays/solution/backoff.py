"""指数退避的等待序列。"""


def delays(base, factor, count, cap=None):
    """返回 count 个等待秒数：base, base*factor, base*factor**2, ...

    给了 cap 就把每一项封顶到 cap。count 为 0 返回空列表。
    base、factor 或 count 为负时抛 ValueError。
    """
    if base < 0 or factor < 0 or count < 0:
        raise ValueError("base, factor and count must not be negative")
    result = []
    for index in range(count):
        value = base * factor**index
        result.append(min(value, cap) if cap is not None else value)
    return result
