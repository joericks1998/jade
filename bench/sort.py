# Bubble sort benchmark — same algorithm and input as sort.jde
# Uses an identical pure-Python bubble sort (no built-ins) for a fair comparison.

def bubble_sort(arr):
    n = len(arr)
    i = 0
    while i < n - 1:
        j = 0
        while j < n - i - 1:
            if arr[j] > arr[j + 1]:
                arr[j], arr[j + 1] = arr[j + 1], arr[j]
            j += 1
        i += 1
    return arr

data = list(range(200, 0, -1))  # [200, 199, ..., 1]
sorted_data = bubble_sort(data)

# Sanity check
assert sorted_data[0] == 1
assert sorted_data[-1] == 200
