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

f0 = factorial(0)    # 1
f1 = factorial(1)    # 1
f5 = factorial(5)    # 120
f7 = factorial(7)    # 5040

fib0  = fib(0)       # 0
fib1  = fib(1)       # 1
fib10 = fib(10)      # 55

s0  = sum_to(0)      # 0
s10 = sum_to(10)     # 55
