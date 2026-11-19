;;; The suite's cases Vali is known to get wrong, so the rest still gate the
;;; build. Each entry is (key reason). The key is how the shim names a case:
;;; the expression as `write` prints it, then " => " and the expected value
;;; (a test-error case is just the expression). The shim prints the key of
;;; every failure in this form, to copy here. A case listed here that
;;; passes fails the build, so take its entry out when you fix it.
;;;
;;; "gap" means Vali is wrong; "upstream" means the suite's expectation is
;;; chibi's, not R7RS's, and Vali's answer is allowed.

;;; The fewest cases that must pass, so a suite that quietly shrinks (a
;;; form that stops running part of the file, say) fails instead of passing.
(define minimum-passed 1150)

(define known-failures
  '(
    ;; 6.2 Numbers
    ("(real? -2.5+0.0i) => #t"
     "#48: gap: -2.5+0i reads as -2.5+0.0i, so real? is false; R7RS 6.2.6 says an exact zero imaginary part makes it real")
    ("(sqrt -1.0-0.0i) => 0.0+1.0i"
     "upstream: -1.0-0.0i is below the branch cut, so 0.0-1.0i, which Vali returns, is the C99 answer; R7RS does not say")
    ("(string->number \"#x4000000000000001/3e1\") => #f"
     "upstream: e is a hex digit, so this is 0x4000000000000001/0x3e1 and Vali's rational is right")

    ;; 6.2 Numbers, number->string formats
    ("(member? -1.7976931348623157e308 (\"-1.7976931348623157e+308\" \"-inf.0\")) => #t"
     "upstream: Vali prints -1.7976931348623157e308, with no +, which is valid R7RS")
    ("(member? 5e-324 (\"4.940656458412465e-324\" \"4.94065645841247e-324\" \"5.0e-324\" \"0.0\")) => #t"
     "upstream: Vali prints the shortest round-trip form, 5e-324")
    ("(member? 1e-323 (\"9.881312916824931e-324\" \"9.88131291682493e-324\" \"1.0e-323\" \"0.0\")) => #t"
     "upstream: Vali prints the shortest round-trip form, 1e-323")
    ("(member? 2e-323 (\"1.976262583364986e-323\" \"1.97626258336499e-323\" \"2.0e-323\" \"0.0\")) => #t"
     "upstream: Vali prints the shortest round-trip form, 2e-323")
    ("(member? 1.7976931348623157e308 (\"1.7976931348623157e+308\" \"+inf.0\")) => #t"
     "upstream: Vali prints 1.7976931348623157e308, with no +, which is valid R7RS")

    ;; 6.10 Control features
    ("(apply +)"
     "#50: gap: apply with no list argument returns 0 where R7RS 6.10 makes it an error")

    ;; 6.14 System interface: the suite's host grants no capabilities, so
    ;; (scheme time) and (scheme file) raise (ADR 0009).
    ("(real? (current-second)) => #t" "#52: not run: the host grants no clock")
    ("(inexact? (current-second)) => #t" "#52: not run: the host grants no clock")
    ("(exact? (current-jiffy)) => #t" "#52: not run: the host grants no clock")
    ("(file-exists? \".\") => #t" "#52: not run: the host grants no file access")
    ("(file-exists? \" no such file \") => #f" "#52: not run: the host grants no file access")

    ;; Read syntax: write must put bars around symbols that read as numbers
    ("(get-output-string out) => \"|+i|\""
     "#47: gap: write prints the symbol |+i| as +i, which reads back as a number")
    ("(get-output-string out) => \"|-i|\""
     "#47: gap: as above for |-i|")
    ("(get-output-string out) => \"|+inf.0|\""
     "#47: gap: as above for |+inf.0|")
    ("(get-output-string out) => \"|-inf.0|\""
     "#47: gap: as above for |-inf.0|")
    ("(get-output-string out) => \"|+nan.0|\""
     "#47: gap: as above for |+nan.0|")
    ("(get-output-string out) => \"|+NaN.0|\""
     "#47: gap: as above for |+NaN.0|")
    ("(get-output-string out) => \"|+NaN.0abc|\""
     "upstream: +NaN.0abc is not a number, so no bars are needed to read it back; chibi writes them anyway")))
