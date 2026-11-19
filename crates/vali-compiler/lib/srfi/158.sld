;;; (srfi 158): generators and accumulators, in Scheme.
;;;
;;; A generator is a procedure of no arguments that returns the next value
;;; or an end-of-file object; an accumulator is a procedure of one
;;; argument that takes values, and returns its result when given an
;;; end-of-file object.
;;;
;;; Written after the SRFI's sample implementation (by Shiro Kawai, John
;;; Cowan and Thomas Gilray, MIT licence, Copyright (C) 2015). It differs
;;; where the sample is wrong or leaves a case open:
;;; - The generators the SRFI says consume nothing when made (`gmerge`,
;;;   `gmap`, `gcombine`, `gdelete-neighbor-dups`) pull from their sources
;;;   only when called.
;;; - `gtake` reads exactly as many values as it returns, so the source
;;;   can be read on from where it stopped; `gtake-while` stops reading its
;;;   source for good at the first value that fails the predicate.
;;; - `gflatten` skips empty lists, `gmerge` takes any number of
;;;   generators (none gives an empty generator), and `circular-generator`
;;;   with no arguments is an empty generator.
;;; - `make-iota-generator` and `make-range-generator` compute each value
;;;   from the start and the step, so inexact steps do not drift, and give
;;;   inexact values unless both start and step are exact.
;;; - `make-coroutine-generator` suspends the procedure with `call/cc`, and
;;;   `make-unfold-generator` does not need to. A coroutine's procedure
;;;   runs in the dynamic environment of the call that first started it,
;;;   as a continuation resumed with `call/cc` carries its own: its
;;;   `parameterize` bindings and current ports are those of the first
;;;   call to the generator, not the call that resumes it (SRFI 226's
;;;   composable continuations would lift that). What the procedure
;;;   raises is raised again in the call that is running, so a handler
;;;   around any call to the generator sees it, not one around the first
;;;   call; a handler that returns answers a `raise-continuable` and the
;;;   procedure goes on, and an error nothing inside the procedure
;;;   handles finishes the generator. If the caller's handler leaves by
;;;   a jump, the generator is finished at its next call.
;;; - `string->generator` takes the characters it will yield from the
;;;   string when it is made (`string-ref` by index is linear in Vali),
;;;   so a later change to the string is not seen.
;;; - `gmerge` is a balanced tree of two-way merges, so each value costs
;;;   O(log k) for k generators, and the leftmost of equal values wins.

(define-library (srfi 158)
  (import (scheme base) (scheme case-lambda))
  (export generator circular-generator make-iota-generator
          make-range-generator make-coroutine-generator
          list->generator vector->generator reverse-vector->generator
          string->generator bytevector->generator
          make-for-each-generator make-unfold-generator
          gcons* gappend gcombine gfilter gremove
          gtake gdrop gtake-while gdrop-while
          gflatten ggroup gmerge gmap gstate-filter
          gdelete gdelete-neighbor-dups gindex gselect
          generator->list generator->reverse-list
          generator->vector generator->vector! generator->string
          generator-fold generator-map->list generator-for-each
          generator-find generator-count generator-any generator-every
          generator-unfold
          make-accumulator count-accumulator list-accumulator
          reverse-list-accumulator vector-accumulator
          reverse-vector-accumulator vector-accumulator!
          string-accumulator bytevector-accumulator bytevector-accumulator!
          sum-accumulator product-accumulator)
  (begin
    (define (%any-eof? vs)
      (and (pair? vs) (or (eof-object? (car vs)) (%any-eof? (cdr vs)))))

    ;; Calls each generator of `gens`, in order.
    (define (%next-all gens) (map (lambda (g) (g)) gens))

    ;; ---- Constructors

    (define (generator . args)
      (lambda ()
        (if (null? args)
            (eof-object)
            (let ((next (car args)))
              (set! args (cdr args))
              next))))

    (define (circular-generator . args)
      (let ((rest args))
        (lambda ()
          (cond ((null? args) (eof-object))
                (else
                 (when (null? rest) (set! rest args))
                 (let ((next (car rest)))
                   (set! rest (cdr rest))
                   next))))))

    ;; The `i`th number of an arithmetic sequence: exact only when the
    ;; start and the step both are.
    (define (%term start step i)
      (let ((v (+ start (* i step))))
        (if (and (exact? start) (exact? step)) v (inexact v))))

    (define make-iota-generator
      (case-lambda
        ((count) (make-iota-generator count 0 1))
        ((count start) (make-iota-generator count start 1))
        ((count start step)
         (let ((i 0))
           (lambda ()
             (if (< i count)
                 (let ((v (%term start step i)))
                   (set! i (+ i 1))
                   v)
                 (eof-object)))))))

    (define make-range-generator
      (case-lambda
        ((start) (make-range-generator start +inf.0 1))
        ((start end) (make-range-generator start end 1))
        ((start end step)
         (let ((i 0))
           (lambda ()
             (let ((v (%term start step i)))
               (if (< v end)
                   (begin (set! i (+ i 1)) v)
                   (eof-object))))))))

    ;; What `return` is given when the procedure of a coroutine raises:
    ;; the object raised, and whether the procedure is finished (an error
    ;; no handler inside it dealt with) or waits in a handler for the
    ;; caller's answer (a `raise-continuable` can be answered).
    (define-record-type <failure>
      (make-failure obj final?)
      failure?
      (obj failure-obj)
      (final? failure-final?))

    (define (make-coroutine-generator proc)
      (let ((return #f) (resume #f) (done #f) (pending #f))
        (define (yield v)
          (call/cc (lambda (k) (set! resume k) (return v))))
        ;; Runs inside the procedure, at the point of a raise: hands the
        ;; object to the caller, and, if the caller's handler returns,
        ;; returns that value to the raise.
        (define (handler e)
          (call/cc (lambda (k) (set! resume k) (return (make-failure e #f)))))
        (define (start)
          (guard (e (#t (set! done #t) (return (make-failure e #t))))
            (with-exception-handler handler (lambda () (proc yield))))
          (set! done #t)
          (return (eof-object)))
        (lambda ()
          (cond
           ((or done pending)
            ;; Finished, or the caller's handler left a raise unanswered.
            (set! done #t)
            (eof-object))
           (else
            (let loop ((result (call/cc (lambda (r)
                                          (set! return r)
                                          (if resume (resume #f) (start))))))
              (cond
               ((not (failure? result)) result)
               ((failure-final? result) (raise (failure-obj result)))
               (else
                ;; Raise it here, in the caller's dynamic environment. A
                ;; handler that returns answers a `raise-continuable`.
                (set! pending #t)
                (let ((answer (raise-continuable (failure-obj result))))
                  (set! pending #f)
                  (loop (call/cc (lambda (r)
                                   (set! return r)
                                   (resume answer)))))))))))))

    (define (list->generator lst)
      (lambda ()
        (if (null? lst)
            (eof-object)
            (let ((next (car lst)))
              (set! lst (cdr lst))
              next))))

    (define vector->generator
      (case-lambda
        ((vec) (vector->generator vec 0 (vector-length vec)))
        ((vec start) (vector->generator vec start (vector-length vec)))
        ((vec start end)
         (lambda ()
           (if (>= start end)
               (eof-object)
               (let ((next (vector-ref vec start)))
                 (set! start (+ start 1))
                 next))))))

    (define reverse-vector->generator
      (case-lambda
        ((vec) (reverse-vector->generator vec 0 (vector-length vec)))
        ((vec start) (reverse-vector->generator vec start (vector-length vec)))
        ((vec start end)
         (lambda ()
           (if (>= start end)
               (eof-object)
               (begin
                 (set! end (- end 1))
                 (vector-ref vec end)))))))

    (define string->generator
      (case-lambda
        ((str) (string->generator str 0 (string-length str)))
        ((str start) (string->generator str start (string-length str)))
        ((str start end) (list->generator (string->list str start end)))))

    (define bytevector->generator
      (case-lambda
        ((bv) (bytevector->generator bv 0 (bytevector-length bv)))
        ((bv start) (bytevector->generator bv start (bytevector-length bv)))
        ((bv start end)
         (lambda ()
           (if (>= start end)
               (eof-object)
               (let ((next (bytevector-u8-ref bv start)))
                 (set! start (+ start 1))
                 next))))))

    (define (make-for-each-generator for-each obj)
      (make-coroutine-generator (lambda (yield) (for-each yield obj))))

    (define (make-unfold-generator stop? mapper successor seed)
      (lambda ()
        (if (stop? seed)
            (eof-object)
            (let ((v (mapper seed)))
              (set! seed (successor seed))
              v))))

    ;; ---- Operations

    (define (gcons* . args)
      (lambda ()
        (cond ((null? args) (eof-object))
              ((null? (cdr args)) ((car args)))
              (else
               (let ((v (car args)))
                 (set! args (cdr args))
                 v)))))

    (define (gappend . gens)
      (lambda ()
        (let loop ()
          (if (null? gens)
              (eof-object)
              (let ((v ((car gens))))
                (if (eof-object? v)
                    (begin (set! gens (cdr gens)) (loop))
                    v))))))

    (define (gflatten gen)
      (let ((state '()))
        (lambda ()
          (let loop ()
            (cond ((pair? state)
                   (let ((v (car state)))
                     (set! state (cdr state))
                     v))
                  (else
                   (let ((next (gen)))
                     (if (eof-object? next)
                         next
                         (begin (set! state next) (loop))))))))))

    (define ggroup
      (case-lambda
        ((gen k) (%ggroup gen k #f #f))
        ((gen k padding) (%ggroup gen k #t padding))))

    (define (%ggroup gen k pad? padding)
      (lambda ()
        (let loop ((item (gen)) (result '()) (count 0))
          (cond ((eof-object? item)
                 (cond ((null? result) item)
                       (pad? (append (reverse result)
                                     (make-list (- k count) padding)))
                       (else (reverse result))))
                ((= (+ count 1) k) (reverse (cons item result)))
                (else (loop (gen) (cons item result) (+ count 1)))))))

    ;; Merges two generators; on equal values the first one's comes first.
    ;; It reads each only when asked for a value.
    (define (%merge2 less? ga gb)
      (let* ((none (list 'none)) (a none) (b none))
        (lambda ()
          (when (eq? a none) (set! a (ga)))
          (when (eq? b none) (set! b (gb)))
          (cond ((eof-object? a)
                 (let ((v b))
                   (unless (eof-object? v) (set! b none))
                   v))
                ((eof-object? b) (let ((v a)) (set! a none) v))
                ((less? b a) (let ((v b)) (set! b none) v))
                (else (let ((v a)) (set! a none) v))))))

    ;; A balanced tree of two-way merges, in the order of `gens`.
    (define (%merge-tree less? gens n)
      (if (= n 1)
          (car gens)
          (let ((half (quotient n 2)))
            (%merge2 less?
                     (%merge-tree less? gens half)
                     (%merge-tree less? (list-tail gens half) (- n half))))))

    (define (gmerge less? . gens)
      (if (null? gens)
          (generator)
          (%merge-tree less? gens (length gens))))

    (define (gmap proc gen . gens)
      (if (null? gens)
          (lambda ()
            (let ((item (gen)))
              (if (eof-object? item) item (proc item))))
          (let ((gens (cons gen gens)))
            (lambda ()
              (let ((items (%next-all gens)))
                (if (%any-eof? items)
                    (eof-object)
                    (apply proc items)))))))

    (define (gcombine proc seed gen . gens)
      (let ((gens (cons gen gens)))
        (lambda ()
          (let ((items (%next-all gens)))
            (if (%any-eof? items)
                (eof-object)
                (call-with-values
                    (lambda () (apply proc (append items (list seed))))
                  (lambda (value new-seed)
                    (set! seed new-seed)
                    value)))))))

    (define (gfilter pred gen)
      (lambda ()
        (let loop ()
          (let ((next (gen)))
            (if (or (eof-object? next) (pred next))
                next
                (loop))))))

    (define (gremove pred gen)
      (gfilter (lambda (v) (not (pred v))) gen))

    (define (gstate-filter proc seed gen)
      (lambda ()
        (let loop ()
          (let ((item (gen)))
            (if (eof-object? item)
                item
                (call-with-values (lambda () (proc item seed))
                  (lambda (yes new-seed)
                    (set! seed new-seed)
                    (if yes item (loop)))))))))

    (define gtake
      (case-lambda
        ((gen k) (%gtake gen k #f #f))
        ((gen k padding) (%gtake gen k #t padding))))

    (define (%gtake gen k pad? padding)
      (lambda ()
        (if (<= k 0)
            (eof-object)
            (let ((v (gen)))
              (cond ((not (eof-object? v)) (set! k (- k 1)) v)
                    (pad? (set! k (- k 1)) padding)
                    (else (set! k 0) v))))))

    (define (gdrop gen k)
      (lambda ()
        (let loop ()
          (when (> k 0)
            (set! k (- k 1))
            (gen)
            (loop)))
        (gen)))

    (define (gtake-while pred gen)
      (let ((done #f))
        (lambda ()
          (if done
              (eof-object)
              (let ((next (gen)))
                (cond ((eof-object? next) (set! done #t) next)
                      ((pred next) next)
                      (else (set! done #t) (eof-object))))))))

    (define (gdrop-while pred gen)
      (let ((dropping #t))
        (lambda ()
          (if dropping
              (let loop ()
                (let ((v (gen)))
                  (if (and (not (eof-object? v)) (pred v))
                      (loop)
                      (begin (set! dropping #f) v))))
              (gen)))))

    (define gdelete
      (case-lambda
        ((item gen) (gdelete item gen equal?))
        ((item gen =)
         (lambda ()
           (let loop ()
             (let ((v (gen)))
               (if (and (not (eof-object? v)) (= item v))
                   (loop)
                   v)))))))

    (define gdelete-neighbor-dups
      (case-lambda
        ((gen) (gdelete-neighbor-dups gen equal?))
        ((gen =)
         (let ((first? #t) (prev #f))
           (lambda ()
             (if first?
                 (begin (set! first? #f)
                        (set! prev (gen))
                        prev)
                 (let loop ()
                   (let ((v (gen)))
                     (cond ((eof-object? v) v)
                           ((= prev v) (loop))
                           (else (set! prev v) v))))))))))

    (define (gindex value-gen index-gen)
      (let ((done? #f) (count 0))
        (lambda ()
          (if done?
              (eof-object)
              (let loop ((value (value-gen)) (index (index-gen)))
                (cond ((or (eof-object? value) (eof-object? index))
                       (set! done? #t)
                       (eof-object))
                      ((= index count)
                       (set! count (+ count 1))
                       value)
                      (else
                       (set! count (+ count 1))
                       (loop (value-gen) index))))))))

    (define (gselect value-gen truth-gen)
      (let ((done? #f))
        (lambda ()
          (if done?
              (eof-object)
              (let loop ((value (value-gen)) (truth (truth-gen)))
                (cond ((or (eof-object? value) (eof-object? truth))
                       (set! done? #t)
                       (eof-object))
                      (truth value)
                      (else (loop (value-gen) (truth-gen)))))))))

    ;; ---- Consumers

    (define generator->reverse-list
      (case-lambda
        ((gen)
         (let loop ((acc '()))
           (let ((v (gen)))
             (if (eof-object? v) acc (loop (cons v acc))))))
        ((gen k)
         (let loop ((acc '()) (k k))
           (if (<= k 0)
               acc
               (let ((v (gen)))
                 (if (eof-object? v) acc (loop (cons v acc) (- k 1)))))))))

    (define generator->list
      (case-lambda
        ((gen) (reverse (generator->reverse-list gen)))
        ((gen k) (reverse (generator->reverse-list gen k)))))

    (define generator->vector
      (case-lambda
        ((gen) (list->vector (generator->list gen)))
        ((gen k) (list->vector (generator->list gen k)))))

    (define generator->string
      (case-lambda
        ((gen) (list->string (generator->list gen)))
        ((gen k) (list->string (generator->list gen k)))))

    (define (generator->vector! vector at gen)
      (let loop ((at at) (count 0))
        (if (>= at (vector-length vector))
            count
            (let ((v (gen)))
              (if (eof-object? v)
                  count
                  (begin (vector-set! vector at v)
                         (loop (+ at 1) (+ count 1))))))))

    (define (generator-fold proc seed gen . gens)
      (if (null? gens)
          (let loop ((acc seed))
            (let ((v (gen)))
              (if (eof-object? v) acc (loop (proc v acc)))))
          (let ((gens (cons gen gens)))
            (let loop ((acc seed))
              (let ((vs (%next-all gens)))
                (if (%any-eof? vs)
                    acc
                    (loop (apply proc (append vs (list acc))))))))))

    (define (generator-for-each proc gen . gens)
      (let ((gens (cons gen gens)))
        (let loop ()
          (let ((vs (%next-all gens)))
            (unless (%any-eof? vs)
              (apply proc vs)
              (loop))))))

    (define (generator-map->list proc gen . gens)
      (let ((gens (cons gen gens)))
        (let loop ((acc '()))
          (let ((vs (%next-all gens)))
            (if (%any-eof? vs)
                (reverse acc)
                (loop (cons (apply proc vs) acc)))))))

    (define (generator-find pred gen)
      (let loop ()
        (let ((v (gen)))
          (cond ((eof-object? v) #f)
                ((pred v) v)
                (else (loop))))))

    (define (generator-count pred gen)
      (generator-fold (lambda (v n) (if (pred v) (+ n 1) n)) 0 gen))

    (define (generator-any pred gen)
      (let loop ()
        (let ((v (gen)))
          (if (eof-object? v)
              #f
              (or (pred v) (loop))))))

    (define (generator-every pred gen)
      (let loop ((last #t))
        (let ((v (gen)))
          (if (eof-object? v)
              last
              (let ((r (pred v)))
                (and r (loop r)))))))

    (define (generator-unfold gen unfold . args)
      (apply unfold eof-object? (lambda (x) x) (lambda (x) (gen)) (gen) args))

    ;; ---- Accumulators

    (define (make-accumulator kons knil finalize)
      (let ((state knil))
        (lambda (obj)
          (if (eof-object? obj)
              (finalize state)
              (set! state (kons obj state))))))

    (define (count-accumulator)
      (make-accumulator (lambda (obj state) (+ state 1)) 0 (lambda (x) x)))

    (define (list-accumulator) (make-accumulator cons '() reverse))

    (define (reverse-list-accumulator)
      (make-accumulator cons '() (lambda (x) x)))

    (define (vector-accumulator)
      (make-accumulator cons '()
                        (lambda (x) (list->vector (reverse x)))))

    (define (reverse-vector-accumulator)
      (make-accumulator cons '() list->vector))

    (define (vector-accumulator! vec at)
      (lambda (obj)
        (if (eof-object? obj)
            vec
            (begin (vector-set! vec at obj)
                   (set! at (+ at 1))))))

    (define (%list->bytevector lst)
      (let ((bv (make-bytevector (length lst) 0)))
        (let loop ((i 0) (lst lst))
          (if (null? lst)
              bv
              (begin (bytevector-u8-set! bv i (car lst))
                     (loop (+ i 1) (cdr lst)))))))

    (define (bytevector-accumulator)
      (make-accumulator cons '()
                        (lambda (x) (%list->bytevector (reverse x)))))

    (define (bytevector-accumulator! bv at)
      (lambda (obj)
        (if (eof-object? obj)
            bv
            (begin (bytevector-u8-set! bv at obj)
                   (set! at (+ at 1))))))

    (define (string-accumulator)
      (make-accumulator cons '()
                        (lambda (lst) (list->string (reverse lst)))))

    (define (sum-accumulator)
      (make-accumulator + 0 (lambda (x) x)))

    (define (product-accumulator)
      (make-accumulator * 1 (lambda (x) x)))))
