"""指数退避的等待序列。"""


def delays(base, factor, count, cap=None):
    """返回 count 个等待秒数：base, base*factor, base*factor**2, ...

    给了 cap 就把每一项封顶到 cap。count 为 0 返回空列表。
    base、factor 或 count 为负时抛 ValueError。
    """
    raise NotImplementedError
