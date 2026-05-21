#!/bin/bash
# This script generates test data files for testing.

mkdir -p data/

# Generate 13 unique random numbers between 100000 and 999999, then sort them
# We start from 100000 to ensure a consistent number of digits for sorting later
random_numbers=$(shuf -i 100000-999999 -n 13 | sort -n)

i=1
for num in $random_numbers; do
    filename="data/fake_file_${num}.bin"
    dd if=/dev/urandom of="$filename" bs=1M count=40
    echo "Wrote $filename"
    i=$((i+1))
done
