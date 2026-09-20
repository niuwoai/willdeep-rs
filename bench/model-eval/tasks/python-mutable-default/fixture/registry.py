"""登记项目，返回登记后的列表。"""


def add_item(item, items=[]):
    """把 item 追加进 items 并返回。

    不传 items 时每次都从一个新的空列表开始；传了就原地追加并返回同一个列表。
    """
    items.append(item)
    return items
