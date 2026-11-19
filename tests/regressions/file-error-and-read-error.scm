;;; requirement: file-error? and read-error? predicates
;;; found-in: R7RS-small audit
;;; kind: scheme

;; R7RS 6.11 requires file-error? and read-error?, true of the conditions
;; raised when a file can't be opened and when read meets malformed input.
;; Both stay error objects. The test host grants no file capability, so
;; opening a file raises the condition Vali's own (scheme file) raises for
;; that, which counts as a file error.

(import (scheme base) (scheme read) (scheme file) (srfi 64))

(define (caught thunk)
  (guard (e (#t e)) (thunk) 'no-error))

(test-begin "file-error-and-read-error")

(let ((e (caught (lambda () (open-input-file "no-such-file.txt")))))
  (test-assert (error-object? e))
  (test-assert (file-error? e))
  (test-assert (not (read-error? e)))
  (test-assert (string? (error-object-message e)))
  (test-assert (list? (error-object-irritants e))))

(let ((e (caught (lambda () (read (open-input-string "(1 2"))))))
  (test-assert (error-object? e))
  (test-assert (read-error? e))
  (test-assert (not (file-error? e)))
  (test-assert (string? (error-object-message e)))
  (test-assert (list? (error-object-irritants e))))

(let ((e (caught (lambda () (read (open-input-string ")"))))))
  (test-assert (read-error? e)))

;; Other errors, and non-errors, satisfy neither.
(let ((e (caught (lambda () (error "boom" 1 2)))))
  (test-assert (error-object? e))
  (test-assert (not (file-error? e)))
  (test-assert (not (read-error? e))))
(let ((e (caught (lambda () (car 5)))))
  (test-assert (not (file-error? e)))
  (test-assert (not (read-error? e))))
(test-assert (not (file-error? 'oops)))
(test-assert (not (read-error? "oops")))
(test-equal 1 (guard (e ((read-error? e) 1) ((file-error? e) 2))
                (read (open-input-string "(1 2"))))
(test-equal 2 (guard (e ((read-error? e) 1) ((file-error? e) 2))
                (open-output-file "x")))

(test-end)
