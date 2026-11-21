;;; (srfi 151): bitwise operations.
;;;
;;; Integers of any size are two's-complement bit strings. The operations
;;; that need the integer's representation are native: `bitwise-and`,
;;; `-ior`, `-xor`, `-not`, `arithmetic-shift`, `bit-count`,
;;; `integer-length`, `bit-set?`, `first-set-bit`, `copy-bit` and
;;; `bit-field`. They work on fixnums and bignums alike, and a result that
;;; would pass 2^25 bits (a shift of 1 by (expt 10 12), say) is an error
;;; and not an allocation. The rest are written here over them.

(define-library (srfi 151)
  (import (scheme base)
          (only (vali primitive)
                bitwise-and bitwise-ior bitwise-xor bitwise-not
                arithmetic-shift bit-count integer-length bit-set?
                first-set-bit copy-bit bit-field))
  (export bitwise-not bitwise-and bitwise-ior bitwise-xor bitwise-eqv
          bitwise-nand bitwise-nor bitwise-andc1 bitwise-andc2
          bitwise-orc1 bitwise-orc2
          arithmetic-shift bit-count integer-length bitwise-if
          bit-set? copy-bit bit-swap any-bit-set? every-bit-set?
          first-set-bit
          bit-field bit-field-any? bit-field-every?
          bit-field-clear bit-field-set
          bit-field-replace bit-field-replace-same
          bit-field-rotate bit-field-reverse
          bits->list list->bits bits->vector vector->bits bits
          bitwise-fold bitwise-for-each bitwise-unfold
          make-bitwise-generator)
  (begin
    (define (check-integer who i)
      (if (not (exact-integer? i))
          (error (string-append who ": expected an exact integer") i)))

    ;; Associative: a fold from the identity, -1 for `eqv` since
    ;; (bitwise-eqv -1 a) is a.
    (define (bitwise-eqv . is)
      (let loop ((acc -1) (is is))
        (if (null? is)
            acc
            (loop (bitwise-not (bitwise-xor acc (car is))) (cdr is)))))

    (define (bitwise-nand i j) (bitwise-not (bitwise-and i j)))
    (define (bitwise-nor i j) (bitwise-not (bitwise-ior i j)))
    (define (bitwise-andc1 i j) (bitwise-and (bitwise-not i) j))
    (define (bitwise-andc2 i j) (bitwise-and i (bitwise-not j)))
    (define (bitwise-orc1 i j) (bitwise-ior (bitwise-not i) j))
    (define (bitwise-orc2 i j) (bitwise-ior i (bitwise-not j)))

    ;; The bits of `i` where `mask` is 1, and of `j` where it is 0.
    (define (bitwise-if mask i j)
      (bitwise-ior (bitwise-and mask i)
                   (bitwise-and (bitwise-not mask) j)))

    (define (bit-swap index1 index2 i)
      (let ((a (bit-set? index1 i)) (b (bit-set? index2 i)))
        (copy-bit index2 (copy-bit index1 i b) a)))

    (define (any-bit-set? test-bits i)
      (not (zero? (bitwise-and test-bits i))))
    (define (every-bit-set? test-bits i)
      (= test-bits (bitwise-and test-bits i)))

    ;; The mask of the field from `start` up to `end`.
    (define (field-mask start end)
      (arithmetic-shift (- (arithmetic-shift 1 (- end start)) 1) start))

    (define (bit-field-any? i start end)
      (not (zero? (bit-field i start end))))
    (define (bit-field-every? i start end)
      (let ((mask (field-mask start end)))
        (= mask (bitwise-and i mask))))
    (define (bit-field-clear i start end)
      (bitwise-and i (bitwise-not (field-mask start end))))
    (define (bit-field-set i start end)
      (bitwise-ior i (field-mask start end)))
    (define (bit-field-replace dest source start end)
      (bitwise-if (field-mask start end) (arithmetic-shift source start) dest))
    (define (bit-field-replace-same dest source start end)
      (bitwise-if (field-mask start end) source dest))

    (define (bit-field-rotate i count start end)
      (let ((width (- end start)))
        (if (<= width 0)
            (begin (bit-field i start end) i)
            (let* ((c (modulo count width))
                   (field (bit-field i start end))
                   (rotated (bitwise-and
                             (- (arithmetic-shift 1 width) 1)
                             (bitwise-ior (arithmetic-shift field c)
                                          (arithmetic-shift field (- c width))))))
              (bit-field-replace i rotated start end)))))

    (define (bit-field-reverse i start end)
      (let ((width (- end start)))
        (bit-field i start end)
        (let loop ((k 0) (r 0))
          (if (< k width)
              (loop (+ k 1)
                    (if (bit-set? (+ start k) i)
                        (copy-bit (- width k 1) r #t)
                        r))
              (bit-field-replace i r start end)))))

    ;; The optional length: absent, or one exact non-negative integer.
    (define (optional-length who i len)
      (cond ((null? len) (integer-length i))
            ((and (null? (cdr len)) (exact-integer? (car len))
                  (not (negative? (car len))))
             (car len))
            (else (error (string-append who ": expected at most one exact non-negative length")
                         len))))

    (define (bits->list i . len)
      (check-integer "bits->list" i)
      (let ((len (optional-length "bits->list" i len)))
        (let loop ((k (- len 1)) (acc '()))
          (if (< k 0)
              acc
              (loop (- k 1) (cons (bit-set? k i) acc))))))

    (define (bits->vector i . len)
      (check-integer "bits->vector" i)
      (list->vector
       (bits->list i (optional-length "bits->vector" i len))))

    (define (list->bits bools)
      (let loop ((bs (reverse bools)) (acc 0))
        (cond ((null? bs) acc)
              ((boolean? (car bs))
               (loop (cdr bs) (+ (* 2 acc) (if (car bs) 1 0))))
              (else (error "list->bits: expected a boolean" (car bs))))))

    (define (vector->bits v) (list->bits (vector->list v)))
    (define (bits . bools) (list->bits bools))

    (define (bitwise-fold proc seed i)
      (let ((len (integer-length i)))
        (let loop ((k 0) (r seed))
          (if (< k len)
              (loop (+ k 1) (proc (bit-set? k i) r))
              r))))

    (define (bitwise-for-each proc i)
      (let ((len (integer-length i)))
        (let loop ((k 0))
          (if (< k len)
              (begin (proc (bit-set? k i)) (loop (+ k 1)))))))

    (define (bitwise-unfold stop? mapper successor seed)
      (let loop ((state seed) (k 0) (acc 0))
        (if (stop? state)
            acc
            (loop (successor state)
                  (+ k 1)
                  (if (mapper state) (copy-bit k acc #t) acc)))))

    (define (make-bitwise-generator i)
      (let ((k 0))
        (lambda ()
          (let ((b (bit-set? k i)))
            (set! k (+ k 1))
            b))))))
