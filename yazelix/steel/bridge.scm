(provide yzx-helix-start)

(require "bridge-actions.scm")
(require-builtin yazelix/transport)

(define (required-field object key predicate message)
  (let ([value (and (hash? object) (hash-try-get object key))])
    (if (predicate value) value (error message))))

(define (all-strings? values)
  (or (empty? values)
      (and (string? (car values)) (all-strings? (cdr values)))))

(define (non-empty-string-list? value)
  (and (list? value) (not (empty? value)) (all-strings? value)))

;;@doc
;;Validate and dispatch one authenticated transport request.
(define (yzx-helix-handle-request request)
  (define action
    (required-field request 'action string? "request action must be a string"))
  (cond
    [(equal? action "helix.open_files")
     (let ([payload (required-field request 'payload hash? "request payload must be a hash")])
       (apply yzx-helix-open-files
              (cons (required-field payload 'working_dir string? "working_dir must be a string")
                    (required-field payload 'file_paths non-empty-string-list?
                                    "file_paths must be a non-empty list of strings"))))]
    [(equal? action "helix.open_directory")
     (let ([payload (required-field request 'payload hash? "request payload must be a hash")])
       (yzx-helix-open-directory
        (required-field payload 'working_dir string? "working_dir must be a string")
        (required-field payload 'picker_dir string? "picker_dir must be a string")))]
    [else (error "unsupported Helix bridge action")]))

;;@doc
;;Start a caller-owned bridge server with a caller-provided token.
(define (yzx-helix-start auth-token)
  (transport-start auth-token yzx-helix-handle-request))
