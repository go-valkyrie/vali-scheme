;;; requirement: A Rust procedure that fails in a tail call leaves no reference behind
;;; found-in: R7RS conformance suite, run unoptimised
;;; kind: scheme

;; Under precise counting, a Rust procedure called in tail position runs in
;; the frame of the function that called it. When it failed and a handler was
;; installed, the unwinding counted out the frames below the raise
;; procedure's by their functions' slot counts, which is none for a Rust
;; procedure, so its procedure and arguments stayed in the stack, uncounted
;; out. The next function whose frame covered them tripped a debug assertion,
;; and a release build leaked each of the references. The conformance suite's
;; `(test-error (apply + 3))` ran into it, but only when the stack was
;; arranged just so, as it was unoptimised.

(import (scheme base) (srfi 64))

(define (escape thunk)
  (call/cc
   (lambda (k)
     (with-exception-handler
      (lambda (e) (k 'raised))
      (lambda () (call-with-values thunk list))))))

(test-begin "precise-counting-native-tail-call-error")
(test-equal 'raised (escape (lambda () (apply + 3))))
(test-equal 'raised (escape (lambda () (apply + 3 4))))
;; Calls after the unwinding reach the words the failed call left.
(test-equal '(ok) (escape (lambda () 'ok)))
(test-equal 6 (apply + (list 1 2 3)))
(test-end)
