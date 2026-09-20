`registry.py` 的 `add_item` 连着调两次，第二次的返回值里会带着第一次的东西。按 docstring 的约定修好它，显式传进来的列表仍然要原地追加。只改 `registry.py`，`test_registry.py` 不要动，修完跑 `python3 -m unittest`。
