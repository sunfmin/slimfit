import time

from calc import add, is_adult


def test_adds():
    assert add(2, 3) == 5


def test_adds_again():
    assert add(2, 3) == 5


def test_adult():
    assert is_adult(30)


def test_slow():
    time.sleep(0.6)
    assert add(1, 1) == 2
