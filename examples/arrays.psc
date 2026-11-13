// CAIE arrays with negative lower bounds.
// Run: cargo run -- run examples/arrays.psc
DECLARE numbers : ARRAY[-2:2] OF INTEGER
DECLARE i : INTEGER
FOR i <- -2 TO 2
    numbers[i] <- i * i
NEXT i
OUTPUT numbers[-2], ",", numbers[2]
