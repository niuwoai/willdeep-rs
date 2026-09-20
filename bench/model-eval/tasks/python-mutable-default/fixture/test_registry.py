import unittest

from registry import add_item


class AddItemTest(unittest.TestCase):
    def test_each_call_without_a_list_starts_fresh(self):
        self.assertEqual(add_item("a"), ["a"])
        self.assertEqual(add_item("b"), ["b"])

    def test_explicit_list_is_appended_in_place(self):
        shared = []
        self.assertIs(add_item("x", shared), shared)
        add_item("y", shared)
        self.assertEqual(shared, ["x", "y"])


if __name__ == "__main__":
    unittest.main()
