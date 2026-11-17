;;; (scheme file): files (R7RS 6.13). Files are not ambient authority
;;; (ADR 0009 #5 and #6): importing this library always succeeds, but each
;;; procedure needs the file capability, which no evaluation is given yet,
;;; so it raises a condition saying so.

(define-library (scheme file)
  (import (scheme base))
  (export call-with-input-file call-with-output-file delete-file file-exists?
          open-binary-input-file open-binary-output-file open-input-file
          open-output-file with-input-from-file with-output-to-file)
  (begin
    (define (%needs-files name)
      (lambda args
        (error (string-append
                (symbol->string name)
                ": needs the file capability, which this program was not given"))))

    (define call-with-input-file (%needs-files 'call-with-input-file))
    (define call-with-output-file (%needs-files 'call-with-output-file))
    (define delete-file (%needs-files 'delete-file))
    (define file-exists? (%needs-files 'file-exists?))
    (define open-binary-input-file (%needs-files 'open-binary-input-file))
    (define open-binary-output-file (%needs-files 'open-binary-output-file))
    (define open-input-file (%needs-files 'open-input-file))
    (define open-output-file (%needs-files 'open-output-file))
    (define with-input-from-file (%needs-files 'with-input-from-file))
    (define with-output-to-file (%needs-files 'with-output-to-file))))
