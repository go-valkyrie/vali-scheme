;;; (srfi 144): flonums.
;;;
;;; Vali's flonums are IEEE 754 binary64 values. Every procedure that does
;;; arithmetic or math on them is a primitive, written over Rust's `f64`
;;; and the `libm` crate, which gives C99's results for -0.0, infinities
;;; and NaNs on every platform. An argument that is not a flonum is a
;;; type error (`(fl+ 1 2.0)` raises; use `flonum` to convert). The
;;; constants are the correctly rounded values. `fl-fast-fl+*` is #f: `fl+*`
;;; is a fused multiply-add computed in software when the hardware has none.

(define-library (srfi 144)
  (import (scheme base)
          (only (vali primitive)
                flonum? fl=? fl<? fl>? fl<=? fl>=? flunordered?
                flinteger? flzero? flpositive? flnegative? flodd?
                fleven? flfinite? flinfinite? flnan? flnormalized?
                fldenormalized? flmax flmin fl+ fl* fl+* fl- fl/ flabs
                flabsdiff flposdiff flsgn flnumerator fldenominator
                flfloor flceiling flround fltruncate flexp flexp2
                flexp-1 flsquare flsqrt flcbrt flhypot flexpt fllog
                fllog1+ fllog2 fllog10 flsin flcos fltan flasin flacos
                flatan flsinh flcosh fltanh flasinh flacosh flatanh
                flquotient flremainder flremquo flgamma flloggamma
                flfirst-bessel flsecond-bessel flerf flerfc fladjacent
                flcopysign make-flonum flinteger-fraction flexponent
                flinteger-exponent flnormalized-fraction-exponent
                flsign-bit))
  (export fl-e fl-1/e fl-e-2 fl-e-pi/4 fl-log2-e fl-log10-e fl-log-2
          fl-1/log-2 fl-log-3 fl-log-pi fl-log-10 fl-1/log-10 fl-pi
          fl-1/pi fl-2pi fl-pi/2 fl-pi/4 fl-pi-squared fl-degree fl-2/pi
          fl-2/sqrt-pi fl-sqrt-2 fl-sqrt-3 fl-sqrt-5 fl-sqrt-10
          fl-1/sqrt-2 fl-cbrt-2 fl-cbrt-3 fl-4thrt-2 fl-phi fl-log-phi
          fl-1/log-phi fl-euler fl-e-euler fl-sin-1 fl-cos-1
          fl-gamma-1/2 fl-gamma-1/3 fl-gamma-2/3 fl-greatest fl-least
          fl-epsilon fl-fast-fl+* fl-integer-exponent-zero
          fl-integer-exponent-nan flonum make-fllog-base flonum? fl=?
          fl<? fl>? fl<=? fl>=? flunordered? flinteger? flzero?
          flpositive? flnegative? flodd? fleven? flfinite? flinfinite?
          flnan? flnormalized? fldenormalized? flmax flmin fl+ fl* fl+*
          fl- fl/ flabs flabsdiff flposdiff flsgn flnumerator
          fldenominator flfloor flceiling flround fltruncate flexp
          flexp2 flexp-1 flsquare flsqrt flcbrt flhypot flexpt fllog
          fllog1+ fllog2 fllog10 flsin flcos fltan flasin flacos flatan
          flsinh flcosh fltanh flasinh flacosh flatanh flquotient
          flremainder flremquo flgamma flloggamma flfirst-bessel
          flsecond-bessel flerf flerfc fladjacent flcopysign make-flonum
          flinteger-fraction flexponent flinteger-exponent
          flnormalized-fraction-exponent flsign-bit)
  (begin
    (define fl-fast-fl+* #f)
    (define fl-integer-exponent-zero -2147483648)
    (define fl-integer-exponent-nan -2147483648)
    (define fl-e 2.718281828459045)
    (define fl-1/e 0.36787944117144233)
    (define fl-e-2 7.38905609893065)
    (define fl-e-pi/4 2.1932800507380155)
    (define fl-log2-e 1.4426950408889634)
    (define fl-log10-e 0.4342944819032518)
    (define fl-log-2 0.6931471805599453)
    (define fl-1/log-2 1.4426950408889634)
    (define fl-log-3 1.0986122886681098)
    (define fl-log-pi 1.1447298858494002)
    (define fl-log-10 2.302585092994046)
    (define fl-1/log-10 0.4342944819032518)
    (define fl-pi 3.141592653589793)
    (define fl-1/pi 0.3183098861837907)
    (define fl-2pi 6.283185307179586)
    (define fl-pi/2 1.5707963267948966)
    (define fl-pi/4 0.7853981633974483)
    (define fl-pi-squared 9.869604401089358)
    (define fl-degree 0.017453292519943295)
    (define fl-2/pi 0.6366197723675814)
    (define fl-2/sqrt-pi 1.1283791670955126)
    (define fl-sqrt-2 1.4142135623730951)
    (define fl-sqrt-3 1.7320508075688772)
    (define fl-sqrt-5 2.23606797749979)
    (define fl-sqrt-10 3.1622776601683795)
    (define fl-1/sqrt-2 0.7071067811865476)
    (define fl-cbrt-2 1.2599210498948732)
    (define fl-cbrt-3 1.4422495703074083)
    (define fl-4thrt-2 1.189207115002721)
    (define fl-phi 1.618033988749895)
    (define fl-log-phi 0.48121182505960347)
    (define fl-1/log-phi 2.0780869212350273)
    (define fl-euler 0.5772156649015329)
    (define fl-e-euler 1.7810724179901979)
    (define fl-sin-1 0.8414709848078965)
    (define fl-cos-1 0.5403023058681398)
    (define fl-gamma-1/2 1.7724538509055159)
    (define fl-gamma-1/3 2.678938534707747)
    (define fl-gamma-2/3 1.3541179394264004)
    (define fl-greatest 1.7976931348623157e308)
    (define fl-least 4.9406564584124654e-324)
    (define fl-epsilon 2.220446049250313e-16)

    ;; An exact or inexact real as a flonum.
    (define (flonum x)
      (if (real? x)
          (inexact x)
          (error "flonum: expected a real number" x)))

    ;; The base-`x` logarithm, for `x` greater than 1.0.
    (define (make-fllog-base x)
      (if (not (and (flonum? x) (fl>? x 1.0)))
          (error "make-fllog-base: expected a flonum greater than 1.0" x))
      (let ((lx (fllog x)))
        (lambda (y) (fl/ (fllog y) lx))))))
