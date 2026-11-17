;;; (scheme char): R7RS's character library (R7RS 6.6, 6.7): classes,
;;; case mappings, and comparisons that ignore case.

(define-library (scheme char)
  (import (vali primitive))
  (export char-alphabetic? char-numeric? char-whitespace?
          char-upper-case? char-lower-case? digit-value
          char-upcase char-downcase char-foldcase
          char-ci=? char-ci<? char-ci>? char-ci<=? char-ci>=?
          string-upcase string-downcase string-foldcase
          string-ci=? string-ci<? string-ci>? string-ci<=? string-ci>=?))
