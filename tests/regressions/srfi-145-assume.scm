;;; requirement: (srfi 145) assume is available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 145))

(test-begin "srfi-145")
(test-assert (memq 'srfi-145 (features)))
(test-equal 3 (assume 3))
(test-equal 'ok (assume 'ok "it is true"))
(test-equal 'raised
  (guard (e ((error-object? e) 'raised))
    (assume (= 1 2) "one is not two")))
(test-equal "invalid assumption"
  (guard (e ((error-object? e) (error-object-message e)))
    (assume #f)))
(test-end)
