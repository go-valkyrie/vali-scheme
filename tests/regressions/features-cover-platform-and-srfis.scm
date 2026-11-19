;;; requirement: features and cond-expand report srfi-N, platform and endianness identifiers
;;; found-in: R7RS audit
;;; kind: scheme

;; `(features)` listed only the language: no srfi-N for the SRFIs that
;; ship, nothing about the operating system, architecture or byte order,
;; and no version. cond-expand, in programs and in libraries, must agree
;; with `(features)`.

(define-library (probe)
  (import (scheme base))
  (export probe)
  (cond-expand
    (srfi-1 (begin (define probe 'srfi-1)))
    (else (begin (define probe 'none)))))

(import (scheme base) (scheme eval) (srfi 64) (probe))

(define (starts-with? prefix symbol)
  (let ((s (symbol->string symbol)) (n (string-length prefix)))
    (and (>= (string-length s) n) (string=? (substring s 0 n) prefix))))

(define (any-feature? pred)
  (let loop ((fs (features)))
    (and (pair? fs) (or (pred (car fs)) (loop (cdr fs))))))

(test-begin "features-cover-platform-and-srfis")

;; The SRFIs that ship.
(test-assert (memq 'srfi-1 (features)))
(test-assert (memq 'srfi-64 (features)))
(test-assert (memq 'srfi-88 (features)))
(test-assert (memq 'srfi-128 (features)))
(test-assert (memq 'srfi-132 (features)))
(test-assert (not (memq 'srfi-9999 (features))))
(test-equal 'srfi-1 probe)
(test-equal 'yes (cond-expand (srfi-132 'yes) (else 'no)))
(test-equal 'no (cond-expand (srfi-9999 'yes) (else 'no)))

;; Byte order: exactly one.
(test-equal 1
  (+ (if (memq 'big-endian (features)) 1 0)
     (if (memq 'little-endian (features)) 1 0)))

;; An operating system (R7RS appendix B) and an architecture.
(test-assert
  (any-feature?
    (lambda (f) (memq f '(posix unix darwin linux gnu-linux windows bsd)))))
(test-assert
  (any-feature?
    (lambda (f) (memq f '(x86-64 i386 aarch64 arm riscv64 wasm32 ppc sparc)))))

;; The version.
(test-assert (any-feature? (lambda (f) (starts-with? "vali-" f))))

;; cond-expand agrees with `(features)`.
(test-assert
  (let loop ((fs (features)))
    (or (null? fs)
        (and (eval `(cond-expand (,(car fs) #t) (else #f))
                   (environment '(scheme base)))
             (loop (cdr fs))))))
(test-end)
