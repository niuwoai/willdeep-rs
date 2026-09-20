import unittest

from backoff import delays


class DelaysTest(unittest.TestCase):
    def test_grows_geometrically(self):
        self.assertEqual(delays(1, 2, 4), [1, 2, 4, 8])
        self.assertEqual(delays(0.5, 3, 3), [0.5, 1.5, 4.5])

    def test_cap_limits_every_item(self):
        self.assertEqual(delays(1, 2, 5, cap=5), [1, 2, 4, 5, 5])

    def test_zero_count_is_empty(self):
        self.assertEqual(delays(1, 2, 0), [])

    def test_negative_inputs_are_rejected(self):
        for args in [(-1, 2, 3), (1, -2, 3), (1, 2, -3)]:
            with self.assertRaises(ValueError):
                delays(*args)


if __name__ == "__main__":
    unittest.main()
