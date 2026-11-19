;;; (vali hash-nesting): the bound that keeps the hash of a collection that
;;; holds collections finite and cheap. (srfi 113), (srfi 146) and
;;; (srfi 146 hash) share it, so a set that holds a hashmap that holds the
;;; set is bounded as one chain, not once per library.
;;;
;;; A collection being hashed hashes its elements; an element may be
;;; another collection, or the same one again. So the hash of a collection
;;; goes through `hash-nested`: the outermost collection is hashed in full;
;;; one inside it only while it has at most `%full-size` distinct elements
;;; and the nesting is less than `%max-depth` deep; otherwise it counts by
;;; its size alone. Data that contains itself therefore ends, a set of n
;;; sets of two elements (a table of such keys) still hashes by content, and
;;; a collection of n boxes that each hold it costs one element hash a box.
;;; The rule looks only at the depth and the size of the collection in
;;; hand, never at what has been seen, so equal collections hash alike.

(define-library (vali hash-nesting)
  (import (scheme base))
  (export hash-nested)
  (begin
    (define %max-depth 3)
    (define %full-size 16)

    ;; How many collections are being hashed, outermost first.
    (define %depth 0)

    ;; The hash of a collection of `size` distinct elements: what `compute`
    ;; returns when it is to be hashed in full, otherwise `size`.
    (define (hash-nested size compute)
      (if (or (>= %depth %max-depth)
              (and (> %depth 0) (> size %full-size)))
          size
          (dynamic-wind
           (lambda () (set! %depth (+ %depth 1)))
           compute
           (lambda () (set! %depth (- %depth 1)))))))
  )
