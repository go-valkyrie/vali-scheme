;;; (srfi 143): fixnums.
;;;
;;; Vali's fixnums are 61-bit two's-complement integers: `fx-width` is 61,
;;; `fx-greatest` 2^60 - 1 and `fx-least` -2^60 (ADR 0002). Every
;;; procedure is a primitive. The SRFI calls a non-fixnum argument, or a
;;; result outside the fixnum range, an error that need not be signalled;
;;; Vali always signals it. `(fx+ fx-greatest 1)` raises "fixnum overflow"
;;; and never wraps around or returns a bignum, and `(fx+ 1.5 1)` is a
;;; type error. `fxarithmetic-shift` takes counts up to 60 in either
;;; direction, and the bit-field rotation and reversal take fields within
;;; the 61 bits.

(define-library (srfi 143)
  (import (scheme base)
          (only (vali primitive)
                fixnum? fx=? fx<? fx>? fx<=? fx>=?
                fxzero? fxpositive? fxnegative? fxodd? fxeven? fxmax fxmin
                fx+ fx- fxneg fx* fxquotient fxremainder fxabs fxsquare
                fxsqrt fx+/carry fx-/carry fx*/carry
                fxnot fxand fxior fxxor
                fxarithmetic-shift fxarithmetic-shift-left
                fxarithmetic-shift-right
                fxbit-count fxlength fxif fxbit-set? fxcopy-bit
                fxfirst-set-bit fxbit-field fxbit-field-rotate
                fxbit-field-reverse))
  (export fx-width fx-greatest fx-least
          fixnum? fx=? fx<? fx>? fx<=? fx>=?
          fxzero? fxpositive? fxnegative? fxodd? fxeven? fxmax fxmin
          fx+ fx- fxneg fx* fxquotient fxremainder fxabs fxsquare
          fxsqrt fx+/carry fx-/carry fx*/carry
          fxnot fxand fxior fxxor
          fxarithmetic-shift fxarithmetic-shift-left
          fxarithmetic-shift-right
          fxbit-count fxlength fxif fxbit-set? fxcopy-bit
          fxfirst-set-bit fxbit-field fxbit-field-rotate
          fxbit-field-reverse)
  (begin
    (define fx-width 61)
    (define fx-greatest 1152921504606846975)
    (define fx-least -1152921504606846976)))
