// CAIE factorial with explicit function and return types.
// Run: cargo run -- run examples/functions.psc
FUNCTION Factorial(n : INTEGER) RETURNS INTEGER
    IF n = 0 THEN
        RETURN 1
    ELSE
        RETURN n * Factorial(n - 1)
    ENDIF
ENDFUNCTION
OUTPUT Factorial(10)
