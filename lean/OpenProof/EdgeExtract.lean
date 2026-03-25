import Mathlib

/-!
  Extract dependency edges from all Mathlib declarations.
  Output: JSON lines to stdout, one edge per line.
  Usage: lake env lean OpenProof/EdgeExtract.lean > edges.jsonl
-/

open Lean in
unsafe def main : IO Unit := do
  Lean.initSearchPath (← Lean.findSysroot)
  let env ← Lean.importModules #[{ module := `Mathlib }] {} 0

  let stdout ← IO.getStdout
  let edgeCount ← IO.mkRef (0 : Nat)

  -- Collect relevant names into a HashSet for O(1) lookup
  let relevantRef ← IO.mkRef (Lean.NameHashSet.empty)
  env.constants.forM fun name _ci => do
    let s := name.toString
    if !name.isInternal && (s.startsWith "Mathlib." || s.startsWith "Batteries." ||
        s.startsWith "Aesop." || s.startsWith "Init.") then
      relevantRef.modify (·.insert name)

  let relevant ← relevantRef.get
  IO.eprintln s!"Collected {relevant.size} relevant declarations"

  -- Extract edges
  env.constants.forM fun fromName ci => do
    if !relevant.contains fromName then return ()

    let mut used := ci.type.getUsedConstantsAsSet
    match ci.value? with
    | some val => used := used.union val.getUsedConstantsAsSet
    | none => pure ()

    for depName in used.toList do
      if depName == fromName then continue
      if !relevant.contains depName then continue

      let fromParts := fromName.toString.splitOn "."
      let depParts := depName.toString.splitOn "."
      let fromModule := String.intercalate "_" fromParts.dropLast
      let fromLocal := fromParts.getLast!
      let depModule := String.intercalate "_" depParts.dropLast
      let depLocal := depParts.getLast!
      let fromKey := s!"library-seed/mathlib/{fromModule}/{fromLocal}"
      let depKey := s!"library-seed/mathlib/{depModule}/{depLocal}"

      stdout.putStrLn s!"\{\"from\":\"{fromKey}\",\"to\":\"{depKey}\"}"
      edgeCount.modify (· + 1)

  let count ← edgeCount.get
  IO.eprintln s!"Extracted {count} edges"
