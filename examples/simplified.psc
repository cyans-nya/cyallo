// Declaration-free CAIE syntax, sparse holes, and array length.
// Run: cargo run -- run examples/simplified.psc --dialect simplified
Count <- 0
Arr[10] <- 123
FOR i <- 0 TO 2
    Count <- Count + 1
NEXT i
Arr[0] <- Count
OUTPUT Count, ":", LENGTH(Arr), ":", Arr[0], ":", Arr[10]
