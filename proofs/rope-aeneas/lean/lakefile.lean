import Lake
open Lake DSL

-- Aeneas' Lean library from its checkout (see ../README.md for the tool versions).
require aeneas from "/Users/romanthimian/Tools/aeneas/backends/lean"

package «rope_aeneas» {}

-- The code Aeneas generated from ../src/lib.rs, untouched.
lean_lib RopeAeneas

-- The proofs about it.
@[default_target] lean_lib Proofs
