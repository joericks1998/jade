# Extreme benchmark: naive recursive Fibonacci — same algorithm as extreme.jde
# fib(40) requires ~330 million recursive calls.

import sys
sys.setrecursionlimit(100)  # fib(40) max depth is 40, well within default 1000

def fib(n):
    if n <= 1:
        return n
    return fib(n - 1) + fib(n - 2)

result = fib(40)  # 102334155
assert result == 102334155
