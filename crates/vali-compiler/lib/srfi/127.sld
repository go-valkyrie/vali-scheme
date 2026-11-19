;;; (srfi 127): lazy sequences, in Scheme.
;;;
;;; An lseq is a proper list, or a dotted list whose last cdr is a
;;; generator: a procedure of no arguments that returns the next element,
;;; or an end-of-file object. `lseq-cdr` calls the generator and splices
;;; the element in, so each element is generated once and a realized
;;; prefix is an ordinary list.
;;;
;;; Adapted from the SRFI's sample implementation by John Cowan (MIT
;;; licence, Copyright (C) John Cowan 2016). `lseq-take-while` and
;;; `lseq-drop-while` stop at the end of a finite lseq, where the sample
;;; reads past it.
;;;
;;; `lseq?` cannot see how many arguments a procedure takes, so it
;;; accepts any procedure in the last cdr, as the SRFI allows, and says #f
;;; for a circular list.
;;;
;;; The procedures that make a lazy lseq (`lseq-take`, `lseq-map`,
;;; `lseq-filter`, `lseq-take-while`, `lseq->generator`) read the source
;;; only as far as the elements asked for: each steps its source on from
;;; the last element it returned when it is called for the next, not as it
;;; returns one. So a take of three elements of a filtered port reads no
;;; further than the third match.

(define-library (srfi 127)
  (import (scheme base) (scheme case-lambda))
  (export generator->lseq lseq? lseq=?
          lseq-car lseq-first lseq-cdr lseq-rest lseq-ref lseq-take lseq-drop
          lseq-realize lseq->generator lseq-length lseq-append lseq-zip
          lseq-map lseq-for-each lseq-filter lseq-remove
          lseq-find lseq-find-tail lseq-take-while lseq-drop-while
          lseq-any lseq-every lseq-index lseq-member lseq-memq lseq-memv)
  (begin
    ;; True if any of `lists` is empty.
    (define (%any-null? lists)
      (cond ((null? lists) #f)
            ((null? (car lists)) #t)
            (else (%any-null? (cdr lists)))))

    ;; A generator that yields what each of `gens` does, in turn.
    (define (%gappend gens)
      (lambda ()
        (let loop ()
          (if (null? gens)
              (eof-object)
              (let ((v ((car gens))))
                (if (eof-object? v)
                    (begin (set! gens (cdr gens)) (loop))
                    v))))))

    (define (generator->lseq gen)
      (let ((value (gen)))
        (if (eof-object? value)
            '()
            (cons value gen))))

    (define (lseq-car lseq) (car lseq))
    (define (lseq-first lseq) (car lseq))

    (define (lseq-cdr lseq)
      (if (procedure? (cdr lseq))
          (let ((obj ((cdr lseq))))
            (if (eof-object? obj)
                (begin (set-cdr! lseq '()) '())
                (let ((result (cons obj (cdr lseq))))
                  (set-cdr! lseq result)
                  result)))
          (cdr lseq)))

    (define (lseq-rest lseq) (lseq-cdr lseq))

    (define (lseq? obj)
      ;; `slow` follows `obj` at half the speed: they meet on a cycle.
      (let loop ((obj obj) (slow obj) (step? #f))
        (cond ((null? obj) #t)
              ((not (pair? obj)) #f)
              ((procedure? (cdr obj)) #t)
              (else
               (let ((next (cdr obj))
                     (slow (if step? (cdr slow) slow)))
                 (and (not (eq? next slow))
                      (loop next slow (not step?))))))))

    (define (lseq=? = lseq1 lseq2)
      (let loop ((a lseq1) (b lseq2))
        (cond ((eq? a b) #t)
              ((and (null? a) (null? b)) #t)
              ((or (null? a) (null? b)) #f)
              ((= (lseq-car a) (lseq-car b))
               (loop (lseq-cdr a) (lseq-cdr b)))
              (else #f))))

    (define (lseq-take lseq i)
      (let ((first? #t))
        (generator->lseq
         (lambda ()
           (cond ((= i 0) (eof-object))
                 (else
                  (if first? (set! first? #f) (set! lseq (lseq-cdr lseq)))
                  (set! i (- i 1))
                  (lseq-car lseq)))))))

    (define (lseq-drop lseq i)
      (let loop ((i i) (lseq lseq))
        (if (= i 0)
            lseq
            (loop (- i 1) (lseq-cdr lseq)))))

    (define (lseq-ref lseq i) (lseq-car (lseq-drop lseq i)))

    (define (lseq-realize lseq)
      (let loop ((next lseq))
        (if (null? next)
            lseq
            (loop (lseq-cdr next)))))

    (define (lseq-length lseq) (length (lseq-realize lseq)))

    (define (lseq->generator lseq)
      (let ((first? #t))
        (lambda ()
          (if first?
              (set! first? #f)
              (unless (null? lseq) (set! lseq (lseq-cdr lseq))))
          (if (null? lseq) (eof-object) (lseq-car lseq)))))

    (define (lseq-append . lseqs)
      (generator->lseq (%gappend (map lseq->generator lseqs))))

    ;; `lseq-cdr`, except that the empty lseq stays empty.
    (define (%safe-cdr obj)
      (if (null? obj) obj (lseq-cdr obj)))

    (define (lseq-map proc . lseqs)
      (let ((first? #t) (done? #f))
        (generator->lseq
         (lambda ()
           (if first? (set! first? #f) (unless done? (set! lseqs (map %safe-cdr lseqs))))
           (cond ((or done? (%any-null? lseqs)) (set! done? #t) (eof-object))
                 (else (apply proc (map lseq-car lseqs))))))))

    (define (lseq-zip . lseqs) (apply lseq-map list lseqs))

    (define lseq-for-each
      (case-lambda
        ((proc) (error "lseq-for-each: missing arguments"))
        ((proc lseq)
         (let loop ((lseq lseq))
           (unless (null? lseq)
             (proc (lseq-car lseq))
             (loop (lseq-cdr lseq)))))
        ((proc . lseqs)
         (let loop ((lseqs lseqs))
           (unless (%any-null? lseqs)
             (apply proc (map lseq-car lseqs))
             (loop (map lseq-cdr lseqs)))))))

    (define (lseq-filter pred lseq)
      ;; `lseq` itself moves on to the last pair returned, so the head of
      ;; the source is not held while the result is read.
      (let ((first? #t))
        (generator->lseq
         (lambda ()
           (let loop ((l (cond (first? lseq)
                               ((null? lseq) '())
                               (else (lseq-cdr lseq)))))
             (cond ((null? l) (set! first? #f) (set! lseq '()) (eof-object))
                   ((pred (lseq-car l)) (set! first? #f) (set! lseq l) (lseq-car l))
                   (else (loop (lseq-cdr l)))))))))

    (define (lseq-remove pred lseq)
      (lseq-filter (lambda (x) (not (pred x))) lseq))

    (define (lseq-find pred lseq)
      (cond ((null? lseq) #f)
            ((pred (lseq-car lseq)) (lseq-car lseq))
            (else (lseq-find pred (lseq-cdr lseq)))))

    (define (lseq-find-tail pred lseq)
      (cond ((null? lseq) #f)
            ((pred (lseq-car lseq)) lseq)
            (else (lseq-find-tail pred (lseq-cdr lseq)))))

    (define (lseq-take-while pred lseq)
      (let ((first? #t) (done? #f))
        (generator->lseq
         (lambda ()
           (cond (done? (eof-object))
                 (else
                  (if first? (set! first? #f) (set! lseq (lseq-cdr lseq)))
                  (cond ((or (null? lseq) (not (pred (lseq-car lseq))))
                         (set! done? #t)
                         (eof-object))
                        (else (lseq-car lseq)))))))))

    (define (lseq-drop-while pred lseq)
      (let loop ((lseq lseq))
        (if (or (null? lseq) (not (pred (lseq-car lseq))))
            lseq
            (loop (lseq-cdr lseq)))))

    (define (lseq-any pred . lseqs)
      (let loop ((lseqs lseqs))
        (if (%any-null? lseqs)
            #f
            (or (apply pred (map lseq-car lseqs))
                (loop (map lseq-cdr lseqs))))))

    (define (lseq-every pred . lseqs)
      (let loop ((lseqs lseqs) (last-result #t))
        (if (%any-null? lseqs)
            last-result
            (let ((result (apply pred (map lseq-car lseqs))))
              (and result (loop (map lseq-cdr lseqs) result))))))

    (define (lseq-index pred . lseqs)
      (let loop ((lseqs lseqs) (n 0))
        (cond ((%any-null? lseqs) #f)
              ((apply pred (map lseq-car lseqs)) n)
              (else (loop (map %safe-cdr lseqs) (+ n 1))))))

    (define lseq-member
      (case-lambda
        ((x lseq) (lseq-member x lseq equal?))
        ((x lseq =)
         (cond ((null? lseq) #f)
               ((= x (lseq-car lseq)) lseq)
               (else (lseq-member x (lseq-cdr lseq) =))))))

    (define (lseq-memv x lseq) (lseq-member x lseq eqv?))
    (define (lseq-memq x lseq) (lseq-member x lseq eq?))))
