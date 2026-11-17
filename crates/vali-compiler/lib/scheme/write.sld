;;; (scheme write): output to the isolate's output buffer, which the host
;;; takes. Ports come later.

(define-library (scheme write)
  (import (vali primitive))
  (export display write newline write-string write-char))
