;;; requirement: A host test can be granted the clock and file reading, scoped to its directory
;;; found-in: R7RS-small audit
;;; kind: host
;;; host: in-memory-loader
;;; grant: clock, read

;; The conformance suite needs (scheme time) and (scheme file) to run, and
;; Vali grants no capability by default (ADR 0009), so a host test asks
;; for them with `grant:`. Files are reachable below the test's own
;; directory and nowhere else, and only for reading.

(import (scheme base) (scheme file) (scheme time) (scheme read) (srfi 64))

(define (raises? thunk)
  (guard (e (#t #t)) (thunk) #f))

(test-begin "host-test-grants-clock-and-files")

(test-assert (real? (current-second)))
(test-assert (inexact? (current-second)))
(test-assert (exact? (current-jiffy)))
(test-assert (<= (current-jiffy) (current-jiffy)))
(test-equal 1000000 (jiffies-per-second))

(test-assert (file-exists? "."))
(test-assert (file-exists? "host-test-grants-clock-and-files.scm"))
(test-assert (not (file-exists? "no-such-file")))
(test-equal '(define included-value 42)
  (call-with-input-file "data/included.scm" (lambda (p) (read p))))

;; Nothing outside the test's directory, and no writing.
(test-assert (raises? (lambda () (file-exists? "../../Cargo.toml"))))
(test-assert (raises? (lambda () (file-exists? "/"))))
(test-assert (raises? (lambda () (open-input-file "../../Cargo.toml"))))
(test-assert (raises? (lambda () (open-output-file "written-by-a-test"))))
(test-assert (raises? (lambda () (delete-file "data/included.scm"))))

(test-end "host-test-grants-clock-and-files")
