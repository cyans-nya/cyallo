// CAIE inclusive FOR bounds and explicit declarations.
// Run: cargo run -- run examples/loops.psc
DECLARE i : INTEGER
DECLARE total : INTEGER
total <- 0
FOR i <- 1 TO 100
    total <- total + i
NEXT i
OUTPUT total
