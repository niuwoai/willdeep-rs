import unittest

from textutil import normalize_whitespace


class NormalizeWhitespaceTest(unittest.TestCase):
    def test_collapses_repeated_spaces(self):
        self.assertEqual(normalize_whitespace("a  b   c"), "a b c")

    def test_tabs_and_newlines_become_one_space(self):
        self.assertEqual(normalize_whitespace("a\tb\n\nc"), "a b c")

    def test_leading_and_trailing_whitespace_is_removed(self):
        self.assertEqual(normalize_whitespace("  a b  "), "a b")
        self.assertEqual(normalize_whitespace("\n"), "")


if __name__ == "__main__":
    unittest.main()
