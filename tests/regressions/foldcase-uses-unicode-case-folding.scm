;;; requirement: R7RS char-foldcase and string-foldcase
;;; found-in: R7RS conformance suite
;;; kind: scheme

;; R7RS 6.6 and 6.7 fold with Unicode's case folding, which is not
;; lowercasing: U+017F (long s) folds to s, final sigma to sigma, and the
;; Cherokee letters to the capitals.

(import (scheme base) (scheme char) (srfi 64))

(test-begin "foldcase-uses-unicode-case-folding")
(test-eqv #\s (char-foldcase #\x17F))
(test-equal "s" (string-foldcase "\x17F;"))
(test-eqv #\x3C3 (char-foldcase #\x3C2))
(test-equal "\x3C3;" (string-foldcase "\x3C2;"))
;; U+1E9E: simple folding is sharp s, full folding is "ss".
(test-eqv #\xDF (char-foldcase #\x1E9E))
(test-equal "ss" (string-foldcase "\x1E9E;"))
(test-equal "strasse" (string-foldcase "Stra\xDF;e"))
;; Cherokee: small letters fold to the capitals.
(test-eqv #\x13A0 (char-foldcase #\xAB70))
(test-eqv #\x13F0 (char-foldcase #\x13F8))
(test-equal "\x13A0;" (string-foldcase "\xAB70;"))
;; The case-insensitive comparisons fold the same way.
(test-assert (char-ci=? #\x17F #\s))
(test-assert (char-ci=? #\x17F #\S))
(test-assert (string-ci=? "\x17F;" "S"))
(test-assert (string-ci=? "\x3C2;" "\x3A3;"))
(test-assert (char-ci=? #\xAB70 #\x13A0))
(test-assert (not (char-ci=? #\a #\b)))
(test-eqv #\a (char-foldcase #\A))
(test-end)
