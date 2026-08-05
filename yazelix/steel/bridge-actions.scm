(provide yzx-helix-open-files yzx-helix-open-directory)

(require (only-in "helix/commands.scm" change-current-directory open))

;;@doc
;;Open files in this Helix instance after adopting the managed workspace.
(define (yzx-helix-open-files working-dir . file-paths)
  (change-current-directory working-dir)
  (apply open file-paths))

;;@doc
;;Open a file picker rooted separately from the managed workspace.
(define (yzx-helix-open-directory working-dir picker-dir)
  (change-current-directory working-dir)
  (open picker-dir))
