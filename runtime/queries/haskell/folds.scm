; Equations fold without their `where` clause, which folds on its own row.
(function
  .
  (_) @fold
  (match)+ @fold)

(bind
  .
  (_) @fold
  (match)+ @fold)

(function
  "where" @fold
  (local_binds) @fold)

(bind
  "where" @fold
  (local_binds) @fold)

[
  (header)
  (data_type)
  (newtype)
  (class)
  (instance)
  (signature)
  (do)
  (case)
  (alternative)
  (list)
  (record)
  (import)+
  [
    (comment)
    (haddock)
  ]+
] @fold
