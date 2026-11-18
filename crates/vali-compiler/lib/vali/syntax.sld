;;; (vali syntax): where syntax objects come from (ADR 0014 #6). The names
;;; and counts are Racket's: the source's identity, as a string; the line,
;;; from 1; the column, from 0; the position in the source, from 1; and how
;;; many characters it covers, counting Unicode scalar values. Each is #f
;;; for syntax with no source, such as code eval built from data.

(define-library (vali syntax)
  (import (only (vali primitive)
                syntax-source syntax-line syntax-column syntax-position
                syntax-span))
  (export syntax-source syntax-line syntax-column syntax-position syntax-span))
