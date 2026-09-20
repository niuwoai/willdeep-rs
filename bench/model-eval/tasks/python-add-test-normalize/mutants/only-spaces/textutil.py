"""变异：只折空格，制表符和换行原样留着，首尾也不去。"""

import re


def normalize_whitespace(text):
    """把任意连续空白（空格、制表符、换行）折成一个空格，并去掉首尾空白。"""
    return re.sub(" +", " ", text)
