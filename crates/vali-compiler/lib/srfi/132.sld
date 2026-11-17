;;; (srfi 132): sort libraries, in Scheme. Every sort is a stable merge
;;; sort, so each unstable name is the stable procedure, and the linear
;;; update forms may return fresh lists. Selection and medians sort too:
;;; correct, though not in linear time.

(define-library (srfi 132)
  (import (scheme base))
  (export list-sorted? vector-sorted?
          list-sort list-stable-sort list-sort! list-stable-sort!
          vector-sort vector-stable-sort vector-sort! vector-stable-sort!
          list-merge list-merge! vector-merge vector-merge!
          list-delete-neighbor-dups list-delete-neighbor-dups!
          vector-delete-neighbor-dups vector-delete-neighbor-dups!
          vector-find-median vector-find-median!
          vector-select! vector-separate!)
  (begin
    ;; The `i`th of a procedure's optional arguments, or `default`.
    (define (%opt opt i default)
      (if (> (length opt) i) (list-ref opt i) default))

    ;; A vector procedure's optional start and end.
    (define (%range v opt)
      (values (%opt opt 0 0) (%opt opt 1 (vector-length v))))

    ;; Stores the elements of `xs` into `v` from `i` on; returns the index
    ;; past the last.
    (define (%fill! v i xs)
      (if (pair? xs)
          (begin (vector-set! v i (car xs)) (%fill! v (+ i 1) (cdr xs)))
          i))

    (define (list-sorted? less? lis)
      (or (null? lis)
          (let loop ((prev (car lis)) (rest (cdr lis)))
            (or (null? rest)
                (and (not (less? (car rest) prev))
                     (loop (car rest) (cdr rest)))))))

    (define (vector-sorted? less? v . opt)
      (let-values (((start end) (%range v opt)))
        (list-sorted? less? (vector->list v start end))))

    ;; Merging takes from `a` unless `b`'s element is strictly less, so
    ;; equal elements keep their order.
    (define (list-merge less? a b)
      (let loop ((a a) (b b) (acc '()))
        (cond ((null? a) (append (reverse acc) b))
              ((null? b) (append (reverse acc) a))
              ((less? (car b) (car a)) (loop a (cdr b) (cons (car b) acc)))
              (else (loop (cdr a) b (cons (car a) acc))))))
    (define list-merge! list-merge)

    (define (list-stable-sort less? lis)
      (let sort ((lis lis) (n (length lis)))
        (cond ((= n 0) '())
              ((= n 1) (list (car lis)))
              (else
               (let ((half (quotient n 2)))
                 (list-merge less?
                             (sort lis half)
                             (sort (list-tail lis half) (- n half))))))))
    (define list-sort list-stable-sort)
    (define list-sort! list-stable-sort)
    (define list-stable-sort! list-stable-sort)

    (define (vector-stable-sort less? v . opt)
      (let-values (((start end) (%range v opt)))
        (list->vector (list-stable-sort less? (vector->list v start end)))))
    (define vector-sort vector-stable-sort)

    (define (vector-stable-sort! v less? . opt)
      (let-values (((start end) (%range v opt)))
        (%fill! v start (list-stable-sort less? (vector->list v start end)))
        (if #f #f)))
    (define vector-sort! vector-stable-sort!)

    (define (vector-merge less? v1 v2 . opt)
      (list->vector
       (list-merge less?
                   (vector->list v1 (%opt opt 0 0) (%opt opt 1 (vector-length v1)))
                   (vector->list v2 (%opt opt 2 0) (%opt opt 3 (vector-length v2))))))

    (define (vector-merge! less? to v1 v2 . opt)
      (%fill! to (%opt opt 0 0)
              (list-merge less?
                          (vector->list v1 (%opt opt 1 0) (%opt opt 2 (vector-length v1)))
                          (vector->list v2 (%opt opt 3 0) (%opt opt 4 (vector-length v2)))))
      (if #f #f))

    ;; Keeps the first of each run of neighbours `same?` says are equal,
    ;; comparing each element with the last one kept.
    (define (list-delete-neighbor-dups same? lis)
      (if (null? lis)
          '()
          (let loop ((kept (car lis)) (rest (cdr lis)) (acc (list (car lis))))
            (cond ((null? rest) (reverse acc))
                  ((same? kept (car rest)) (loop kept (cdr rest) acc))
                  (else (loop (car rest) (cdr rest) (cons (car rest) acc)))))))
    (define list-delete-neighbor-dups! list-delete-neighbor-dups)

    (define (vector-delete-neighbor-dups same? v . opt)
      (let-values (((start end) (%range v opt)))
        (list->vector (list-delete-neighbor-dups same? (vector->list v start end)))))

    ;; Returns the end of the deduplicated range.
    (define (vector-delete-neighbor-dups! same? v . opt)
      (let-values (((start end) (%range v opt)))
        (%fill! v start (list-delete-neighbor-dups same? (vector->list v start end)))))

    (define (vector-find-median less? v knil . mean)
      (let ((mean (if (pair? mean) (car mean) (lambda (a b) (/ (+ a b) 2))))
            (n (vector-length v)))
        (if (= n 0)
            knil
            (let ((sorted (vector-stable-sort less? v))
                  (half (quotient n 2)))
              (if (odd? n)
                  (vector-ref sorted half)
                  (mean (vector-ref sorted (- half 1)) (vector-ref sorted half)))))))

    (define (vector-find-median! less? v knil . mean)
      (vector-stable-sort! v less?)
      (apply vector-find-median less? v knil mean))

    (define (vector-select! less? v k . opt)
      (let-values (((start end) (%range v opt)))
        (vector-stable-sort! v less? start end)
        (vector-ref v (+ start k))))

    (define (vector-separate! less? v k . opt)
      (let-values (((start end) (%range v opt)))
        (vector-stable-sort! v less? start end)))))
