(require "../../yazelix/steel/bridge.scm"
         (only-in "helix/commands.scm" calls))
(define handle (yzx-helix-start "caller-token"))
(handle (hash 'request_id "files"
              'action "helix.open_files"
              'payload (hash 'working_dir "/workspace"
                             'file_paths '("/one" "/two"))))
(handle (hash 'request_id "directory"
              'action "helix.open_directory"
              'payload (hash 'working_dir "/other-workspace"
                             'picker_dir "/picker")))

(assert! (equal? (calls)
                 '(("cd" "/workspace")
                   ("open" "/one" "/two")
                   ("cd" "/other-workspace")
                   ("open" "/picker"))))

(define accepted-calls (calls))
(define (rejected? request)
  (with-handler (lambda (_) #t)
                (begin (handle request) #f)))
(assert! (rejected? (hash 'action "helix.unknown" 'payload (hash))))
(assert! (rejected? (hash 'action "helix.open_files"
                         'payload (hash 'working_dir "/workspace"
                                        'file_paths '("/one" 2)))))
(assert! (equal? accepted-calls (calls)))
