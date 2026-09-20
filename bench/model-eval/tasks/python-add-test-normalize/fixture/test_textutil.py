import unittest

from textutil import normalize_whitespace


class NormalizeWhitespaceTest(unittest.TestCase):
    def test_collapses_repeated_spaces(self):
        self.assertEqual(normalize_whitespace("a  b   c"), "a b c")


if __name__ == "__main__":
    unittest.main()
