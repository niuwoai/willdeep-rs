"""变异：空白都折了，但首尾的那一个空格留着。"""

import re


def normalize_whitespace(text):
    """把任意连续空白（空格、制表符、换行）折成一个空格，并去掉首尾空白。"""
    return re.sub(r"\s+", " ", text)
