"""文本小工具。"""


def normalize_whitespace(text):
    """把任意连续空白（空格、制表符、换行）折成一个空格，并去掉首尾空白。"""
    return " ".join(text.split())
