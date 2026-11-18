;;; Strings and characters: building, scanning and comparing.
;;; expect: 60000
(import (scheme base) (scheme char))
(define (word n) (string-append "item-" (number->string n)))
(define (count-digits s)
  (let loop ((i 0) (n 0))
    (if (= i (string-length s)) n
        (loop (+ i 1) (if (char-numeric? (string-ref s i)) (+ n 1) n)))))
(let loop ((i 0) (total 0))
  (if (= i 20000) total
      (loop (+ i 1) (+ total (count-digits (string-upcase (word (+ 100 (modulo i 900)))))))))
