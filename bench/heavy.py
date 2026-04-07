def factorial(n):
    if n <= 1:
        return 1
    return n * factorial(n - 1)

def fib(n):
    if n <= 1:
        return n
    return fib(n - 1) + fib(n - 2)

def sum_to(n):
    if n <= 0:
        return 0
    return n + sum_to(n - 1)

f15   = factorial(15)   # 1307674368000
fib28 = fib(28)         # 317811
s500  = sum_to(500)     # 125250
