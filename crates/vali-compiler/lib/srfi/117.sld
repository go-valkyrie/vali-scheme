;;; (srfi 117): queues based on lists, in Scheme.
;;;
;;; Written after the SRFI's sample implementation by John Cowan and Alex
;;; Shinn (MIT licence), whose record and its invariant it keeps: either
;;; `first` is the first pair of a list and `last` its last pair, or both
;;; are the empty list. It differs where the sample is wrong or leaves a
;;; case open:
;;; - `list-queue-append!` links the queues' pairs into a new queue,
;;;   skipping empty ones, instead of failing on an empty first queue.
;;; - `list-queue-map!` applies the procedure front to back and rewrites
;;;   the queue's own pairs.
;;; - `list-queue-unfold` and `list-queue-unfold-right` call the mapper
;;;   in seed order, so the unfolded elements come out in that order.
;;; Misuse of an empty queue, or of something that is not a queue, raises
;;; an error.

(define-library (srfi 117)
  (import (scheme base) (scheme case-lambda))
  (export make-list-queue list-queue list-queue-copy
          list-queue-unfold list-queue-unfold-right
          list-queue? list-queue-empty?
          list-queue-front list-queue-back
          list-queue-list list-queue-first-last
          list-queue-add-front! list-queue-add-back!
          list-queue-remove-front! list-queue-remove-back!
          list-queue-remove-all! list-queue-set-list!
          list-queue-append list-queue-append! list-queue-concatenate
          list-queue-map list-queue-map! list-queue-for-each)
  (begin
    (define-record-type <list-queue>
      (%make-list-queue first last)
      list-queue?
      (first %first %set-first!)
      (last %last %set-last!))

    (define (%check who q)
      (unless (list-queue? q)
        (error (string-append who ": not a list queue") q)))

    (define (%check-nonempty who q)
      (%check who q)
      (when (null? (%first q))
        (error (string-append who ": the list queue is empty") q)))

    (define (%last-pair lis)
      (if (null? (cdr lis)) lis (%last-pair (cdr lis))))

    (define (%last-of lis) (if (null? lis) '() (%last-pair lis)))

    (define make-list-queue
      (case-lambda
        ((lis) (%make-list-queue lis (%last-of lis)))
        ((first last) (%make-list-queue first last))))

    (define (list-queue . objs) (make-list-queue objs))

    (define (list-queue-copy q)
      (%check "list-queue-copy" q)
      (make-list-queue (list-copy (%first q))))

    (define list-queue-unfold
      (case-lambda
        ((stop? mapper successor seed)
         (list-queue-unfold stop? mapper successor seed (list-queue)))
        ((stop? mapper successor seed q)
         (let loop ((seed seed) (acc '()))
           (if (stop? seed)
               ;; `acc` holds the elements last first, so pushing them on
               ;; the front one by one leaves the first unfolded in front.
               (begin (for-each (lambda (x) (list-queue-add-front! q x)) acc)
                      q)
               (loop (successor seed) (cons (mapper seed) acc)))))))

    (define list-queue-unfold-right
      (case-lambda
        ((stop? mapper successor seed)
         (list-queue-unfold-right stop? mapper successor seed (list-queue)))
        ((stop? mapper successor seed q)
         (let loop ((seed seed) (acc '()))
           (if (stop? seed)
               ;; `acc` holds the last unfolded first, so adding at the
               ;; back leaves the first unfolded nearest the back.
               (begin (for-each (lambda (x) (list-queue-add-back! q x)) acc)
                      q)
               (loop (successor seed) (cons (mapper seed) acc)))))))

    (define (list-queue-empty? q)
      (%check "list-queue-empty?" q)
      (null? (%first q)))

    (define (list-queue-front q)
      (%check-nonempty "list-queue-front" q)
      (car (%first q)))

    (define (list-queue-back q)
      (%check-nonempty "list-queue-back" q)
      (car (%last q)))

    (define (list-queue-list q)
      (%check "list-queue-list" q)
      (%first q))

    (define (list-queue-first-last q)
      (%check "list-queue-first-last" q)
      (values (%first q) (%last q)))

    (define (list-queue-add-front! q x)
      (%check "list-queue-add-front!" q)
      (let ((new (cons x (%first q))))
        (when (null? (%first q)) (%set-last! q new))
        (%set-first! q new)))

    (define (list-queue-add-back! q x)
      (%check "list-queue-add-back!" q)
      (let ((new (list x)))
        (if (null? (%first q))
            (%set-first! q new)
            (set-cdr! (%last q) new))
        (%set-last! q new)))

    (define (list-queue-remove-front! q)
      (%check-nonempty "list-queue-remove-front!" q)
      (let* ((old (%first q)) (rest (cdr old)))
        (when (null? rest) (%set-last! q '()))
        (%set-first! q rest)
        (car old)))

    ;; The pair before the last one, or '() when there is only one pair.
    (define (%penult lis)
      (cond ((null? (cdr lis)) '())
            ((null? (cddr lis)) lis)
            (else (%penult (cdr lis)))))

    (define (list-queue-remove-back! q)
      (%check-nonempty "list-queue-remove-back!" q)
      (let* ((x (car (%last q))) (penult (%penult (%first q))))
        (if (null? penult)
            (%set-first! q '())
            (set-cdr! penult '()))
        (%set-last! q penult)
        x))

    (define (list-queue-remove-all! q)
      (%check "list-queue-remove-all!" q)
      (let ((result (%first q)))
        (%set-first! q '())
        (%set-last! q '())
        result))

    (define list-queue-set-list!
      (case-lambda
        ((q lis)
         (%check "list-queue-set-list!" q)
         (%set-first! q lis)
         (%set-last! q (%last-of lis)))
        ((q first last)
         (%check "list-queue-set-list!" q)
         (%set-first! q first)
         (%set-last! q last))))

    (define (list-queue-concatenate qs)
      (let ((result (list-queue)))
        (for-each
         (lambda (q)
           (%check "list-queue-concatenate" q)
           (for-each (lambda (x) (list-queue-add-back! result x)) (%first q)))
         qs)
        result))

    (define (list-queue-append . qs) (list-queue-concatenate qs))

    (define (list-queue-append! . qs)
      (let ((result (list-queue)))
        (for-each
         (lambda (q)
           (%check "list-queue-append!" q)
           (unless (null? (%first q))
             (if (null? (%first result))
                 (%set-first! result (%first q))
                 (set-cdr! (%last result) (%first q)))
             (%set-last! result (%last q))))
         qs)
        result))

    (define (list-queue-map proc q)
      (%check "list-queue-map" q)
      (make-list-queue (map proc (%first q))))

    (define (list-queue-map! proc q)
      (%check "list-queue-map!" q)
      (let loop ((pairs (%first q)))
        (when (pair? pairs)
          (set-car! pairs (proc (car pairs)))
          (loop (cdr pairs)))))

    (define (list-queue-for-each proc q)
      (%check "list-queue-for-each" q)
      (for-each proc (%first q)))))
