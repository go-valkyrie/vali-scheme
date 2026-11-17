;;; (scheme char): R7RS's character library, as much of it as Vali has.

(define-library (scheme char)
  (import (vali primitive))
  (export char-upcase char-downcase char-alphabetic? char-numeric?
          char-whitespace? string-upcase string-downcase))
