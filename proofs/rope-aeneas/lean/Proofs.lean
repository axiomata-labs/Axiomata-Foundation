import Aeneas
import RopeAeneas

/-! The rope's invariant for Aeneas + Lean (Axiomata-OS `docs/plans/rust-core.md` R7, Q6b): proofs about the code Aeneas
generated from `../src/lib.rs` (`RopeAeneas.lean`, untouched). Every line `split_lines` gives holds no break; a leaf
and a branch cache the sums of what they hold, and no addition overflows; a line `replace` joins holds no break and
keeps the cuts on character boundaries. -/
open Aeneas Std Result

-- `if_pos`/`if_neg` step through the generated `if`s; Lean 4.36 calls them deprecated, their suggested
-- replacements say something else.
set_option linter.deprecated false

namespace rope_aeneas

/-- Bytes in a list of lines. -/
def totalBytes (lines : List (alloc.vec.Vec U8)) : Nat := (lines.map (fun l => l.val.length)).sum

theorem totalBytes_take_succ (lines : List (alloc.vec.Vec U8)) (i : Nat) (h : i < lines.length) :
    totalBytes (lines.take (i + 1)) = totalBytes (lines.take i) + lines[i].val.length := by
  simp only [totalBytes, List.take_succ_eq_append_getElem h, List.map_append, List.sum_append, List.map_cons,
    List.map_nil, List.sum_cons, List.sum_nil, Nat.add_zero]

theorem totalBytes_take_le (lines : List (alloc.vec.Vec U8)) (i : Nat) :
    totalBytes (lines.take i) ≤ totalBytes lines := by
  unfold totalBytes
  calc ((lines.take i).map (fun l => l.val.length)).sum
      ≤ ((lines.take i).map (fun l => l.val.length)).sum + ((lines.drop i).map (fun l => l.val.length)).sum :=
        Nat.le_add_right _ _
    _ = (lines.map (fun l => l.val.length)).sum := by
        rw [← List.sum_append, ← List.map_append, List.take_append_drop]

@[step]
theorem leaf_bytes_loop_spec (lines : alloc.vec.Vec (alloc.vec.Vec U8)) (sum i : Usize)
    (hi : i.val ≤ lines.val.length)
    (hsum : sum.val = totalBytes (lines.val.take i.val))
    (hmax : totalBytes lines.val ≤ Usize.max) :
    leaf_bytes_loop lines sum i ⦃ r => r.val = totalBytes lines.val ⦄ := by
  unfold leaf_bytes_loop
  dsimp only
  split
  · step as ⟨ v, hv ⟩
    have hgrow := totalBytes_take_succ lines.val i.val (by scalar_tac)
    have hle := totalBytes_take_le lines.val (i.val + 1)
    step as ⟨ sum1, hsum1 ⟩
    step as ⟨ i1, hi1 ⟩
    step
    simp_all
  · have : i.val = lines.val.length := by scalar_tac
    simp_all
termination_by lines.val.length - i.val
decreasing_by scalar_decr_tac

theorem leaf_bytes_spec (lines : alloc.vec.Vec (alloc.vec.Vec U8))
    (hmax : totalBytes lines.val ≤ Usize.max) :
    leaf_bytes lines ⦃ r => r.val = totalBytes lines.val ⦄ := by
  unfold leaf_bytes
  step
  all_goals simp_all [totalBytes]

/-- The lines a node says it holds. -/
def cachedLines : Node → Nat
  | .Leaf lines _ => lines.val.length
  | .Branch _ lines _ => lines.val

/-- The bytes a node says it holds. -/
def cachedBytes : Node → Nat
  | .Leaf _ bytes => bytes.val
  | .Branch _ _ bytes => bytes.val

/-- Lines the children say they hold. -/
def childLines (children : List Node) : Nat := (children.map cachedLines).sum

/-- Bytes the children say they hold. -/
def childBytes (children : List Node) : Nat := (children.map cachedBytes).sum

theorem sum_take_succ (f : Node → Nat) (l : List Node) (i : Nat) (h : i < l.length) :
    ((l.take (i + 1)).map f).sum = ((l.take i).map f).sum + f l[i] := by
  simp only [List.take_succ_eq_append_getElem h, List.map_append, List.sum_append, List.map_cons,
    List.map_nil, List.sum_cons, List.sum_nil, Nat.add_zero]

theorem sum_take_le (f : Node → Nat) (l : List Node) (i : Nat) :
    ((l.take i).map f).sum ≤ (l.map f).sum := by
  calc ((l.take i).map f).sum ≤ ((l.take i).map f).sum + ((l.drop i).map f).sum := Nat.le_add_right _ _
    _ = (l.map f).sum := by rw [← List.sum_append, ← List.map_append, List.take_append_drop]

@[step]
theorem line_count_spec (n : Node) : line_count n ⦃ r => r.val = cachedLines n ⦄ := by
  unfold line_count; cases n <;> simp [cachedLines]

@[step]
theorem bytes_spec (n : Node) : bytes n ⦃ r => r.val = cachedBytes n ⦄ := by
  unfold bytes; cases n <;> simp [cachedBytes]

@[step]
theorem branch_loop_spec (children : alloc.vec.Vec Node) (lines sum i : Usize)
    (hi : i.val ≤ children.val.length)
    (hlines : lines.val = ((children.val.take i.val).map cachedLines).sum)
    (hsum : sum.val = ((children.val.take i.val).map cachedBytes).sum)
    (hmaxl : childLines children.val ≤ Usize.max)
    (hmaxb : childBytes children.val ≤ Usize.max) :
    branch_loop children lines sum i ⦃ r =>
      r.1.val = childLines children.val ∧ r.2.val = childBytes children.val ⦄ := by
  unfold branch_loop
  dsimp only
  split
  · step as ⟨ n, hn ⟩
    have hl := sum_take_succ cachedLines children.val i.val (by scalar_tac)
    have hb := sum_take_succ cachedBytes children.val i.val (by scalar_tac)
    have hll := sum_take_le cachedLines children.val (i.val + 1)
    have hbl := sum_take_le cachedBytes children.val (i.val + 1)
    simp only [childLines, childBytes] at *
    step as ⟨ c, hc ⟩
    step as ⟨ lines1, hlines1 ⟩
    step as ⟨ b, hb' ⟩
    step as ⟨ sum1, hsum1 ⟩
    step as ⟨ i1, hi1 ⟩
    step
    all_goals simp_all [childLines, childBytes]
  · have : i.val = children.val.length := by scalar_tac
    simp_all [childLines, childBytes]
termination_by children.val.length - i.val
decreasing_by scalar_decr_tac

/-- `Node::branch`: a new branch caches the sums of what its children cache. -/
theorem branch_spec (children : alloc.vec.Vec Node)
    (hmaxl : childLines children.val ≤ Usize.max) (hmaxb : childBytes children.val ≤ Usize.max) :
    branch children ⦃ r => cachedLines r = childLines children.val ∧ cachedBytes r = childBytes children.val ⦄ := by
  unfold branch
  step as ⟨ lines, bytes, h1, h2 ⟩
  all_goals simp_all [cachedLines, cachedBytes]

theorem take_drop_succ (l : List U8) (i n : Nat) (h : i < l.length) :
    (l.drop i).take (n + 1) = l[i] :: (l.drop (i + 1)).take n := by
  rw [List.drop_eq_getElem_cons h, List.take_succ_cons]

@[step]
theorem append_loop_spec (bytes1 : alloc.vec.Vec U8) (extra : Aeneas.Std.Slice U8) (upto i : Usize)
    (hi : i.val ≤ upto.val) (hto : upto.val ≤ extra.length)
    (hcap : bytes1.val.length + (upto.val - i.val) ≤ Usize.max) :
    append_loop bytes1 extra upto i ⦃ r => r.val = bytes1.val ++ (extra.val.drop i.val).take (upto.val - i.val) ⦄ := by
  unfold append_loop
  split
  · step as ⟨ x, hx ⟩
    step as ⟨ b2, hb2 ⟩
    step as ⟨ i1, hi1 ⟩
    step as ⟨ r, hr ⟩
    · simp only [hb2, hi1, List.length_append, List.length_singleton]; scalar_tac
    · have hlt : i.val < upto.val := by scalar_tac
      have hext : i.val < extra.val.length := by simp only [Aeneas.Std.Slice.length] at hto; omega
      have hsplit := take_drop_succ extra.val i.val (upto.val - i.val - 1) hext
      have hn : upto.val - i.val = (upto.val - i.val - 1) + 1 := by omega
      have hn1 : upto.val - i1.val = upto.val - i.val - 1 := by omega
      rw [hr, hb2, hn, hsplit, hn1, hi1, hx]
      simp
  · have : upto.val - i.val = 0 := by scalar_tac
    simp [this]
termination_by upto.val - i.val
decreasing_by scalar_decr_tac

/-- One line of what `replace` builds: `before` up to `start`, the part, `after` from `end`. -/
theorem joined_line_spec (before part after : Aeneas.Std.Slice U8) (start «end» : Usize)
    (hs : start.val ≤ before.length) (he : «end».val ≤ after.length)
    (hcap : start.val + part.length + (after.length - «end».val) ≤ Usize.max) :
    joined_line before start part after «end» ⦃ r =>
      r.val = before.val.take start.val ++ part.val ++ after.val.drop «end».val ⦄ := by
  unfold joined_line append
  step as ⟨ l1, h1 ⟩
  step as ⟨ l2, h2 ⟩
  step as ⟨ l3, h3 ⟩
  all_goals simp_all

/-- A line holds no line break. -/
def noBreak (l : List U8) : Prop := ∀ b ∈ l, b.val ≠ 10 ∧ b.val ≠ 13

/-- The byte at `i` starts a character, or `i` is the end: no UTF-8 continuation byte `10xx_xxxx`. -/
def boundary (s : List U8) (i : Nat) : Prop :=
  i = s.length ∨ ∃ h : i < s.length, ¬ (128 ≤ (s[i]).val ∧ (s[i]).val < 192)

/-- The joined line holds no break when its pieces hold none. -/
theorem joined_noBreak (a p c : List U8) (ha : noBreak a) (hp : noBreak p) (hc : noBreak c)
    (start «end» : Nat) : noBreak (a.take start ++ p ++ c.drop «end») := by
  intro b hb
  simp only [List.mem_append] at hb
  rcases hb with (hb | hb) | hb
  · exact ha b (List.mem_of_mem_take hb)
  · exact hp b hb
  · exact hc b (List.mem_of_mem_drop hb)

/-- Where the part begins, the joined line starts a character when the part does. -/
theorem joined_boundary_start (a p c : List U8) (start «end» : Nat) (hs : start ≤ a.length)
    (hp : p ≠ []) (hb : boundary p 0) : boundary (a.take start ++ p ++ c.drop «end») start := by
  have hlen : (a.take start).length = start := by simp [hs]
  have hpos : 0 < p.length := List.length_pos_iff.mpr hp
  rcases hb with h | ⟨h, hnot⟩
  · omega
  · right
    have hlt : start < (a.take start ++ p ++ c.drop «end»).length := by simp [hlen]; omega
    refine ⟨hlt, ?_⟩
    have heq : (a.take start ++ p ++ c.drop «end»)[start] = p[0] := by
      simp [hlen, hpos]
    rw [heq]; exact hnot

/-- Where the part ends, the joined line starts a character when `after` did at `end`. -/
theorem joined_boundary_end (a p c : List U8) (start «end» : Nat) (hs : start ≤ a.length)
    (hb : boundary c «end») :
    boundary (a.take start ++ p ++ c.drop «end») (start + p.length) := by
  have hlen : (a.take start).length = start := by simp [hs]
  rcases hb with h | ⟨h, hnot⟩
  · left; simp [hlen, h]
  · right
    have hlt : start + p.length < (a.take start ++ p ++ c.drop «end»).length := by simp [hlen]; omega
    refine ⟨hlt, ?_⟩
    have heq : (a.take start ++ p ++ c.drop «end»)[start + p.length] = c[«end»] := by
      simp [hlen]
    rw [heq]; exact hnot

@[step]
theorem split_lines_loop_spec (text : Aeneas.Std.Slice U8) (lines : alloc.vec.Vec (alloc.vec.Vec U8))
    (line : alloc.vec.Vec U8) (i : Usize)
    (hi : i.val ≤ text.length)
    (hcount : lines.val.length + line.val.length ≤ i.val)
    (hmax : text.length < Usize.max)
    (hline : noBreak line.val)
    (hlines : ∀ l ∈ lines.val, noBreak l.val) :
    split_lines_loop text lines line i ⦃ r =>
      (∀ l ∈ r.1.val, noBreak l.val) ∧ noBreak r.2.val ∧ r.1.val.length + r.2.val.length ≤ text.length ⦄ := by
  unfold split_lines_loop
  dsimp only
  split
  · step as ⟨ b, hb ⟩
    have hilt : i.val < text.length := by scalar_tac
    -- What every branch needs once it has pushed the line: the old lines and the line hold no break.
    have pushed : ∀ (lines2 : alloc.vec.Vec (alloc.vec.Vec U8)), lines2.val = lines.val ++ [line] →
        ∀ l ∈ lines2.val, noBreak l.val := by
      intro lines2 hl2 l hl
      rw [hl2] at hl
      simp only [List.mem_append, List.mem_singleton] at hl
      rcases hl with h | h
      · exact hlines l h
      · subst h; exact hline
    have empty : noBreak (alloc.vec.Vec.new U8).val := by simp [noBreak]
    by_cases h10 : b = 10#u8
    · rw [if_pos h10]
      step as ⟨ lines2, hl2 ⟩
      step as ⟨ i3, hi3 ⟩
      step
      · simp only [hl2, hi3, List.length_append, List.length_singleton]; simp; omega
      · exact pushed lines2 hl2
      · exact ⟨‹_›, ‹_›, ‹_›⟩
    · rw [if_neg h10]
      by_cases h13 : b = 13#u8
      · rw [if_pos h13]
        step as ⟨ lines2, hl2 ⟩
        step as ⟨ i3, hi3 ⟩
        by_cases hnext : i3 < text.len
        · rw [if_pos hnext]
          step as ⟨ b6, hb6 ⟩
          by_cases h6 : b6 = 10#u8
          · rw [if_pos h6]
            step as ⟨ i7, hi7 ⟩
            step
            · simp only [hl2, hi7, hi3, List.length_append, List.length_singleton]; simp; omega
            · exact pushed lines2 hl2
            · exact ⟨‹_›, ‹_›, ‹_›⟩
          · rw [if_neg h6]
            step as ⟨ i7, hi7 ⟩
            step
            · simp only [hl2, hi7, List.length_append, List.length_singleton]; simp; omega
            · exact pushed lines2 hl2
            · exact ⟨‹_›, ‹_›, ‹_›⟩
        · rw [if_neg hnext]
          step as ⟨ i7, hi7 ⟩
          step
          · simp only [hl2, hi7, List.length_append, List.length_singleton]; simp; omega
          · exact pushed lines2 hl2
          · exact ⟨‹_›, ‹_›, ‹_›⟩
      · rw [if_neg h13]
        step as ⟨ line2, hl2 ⟩
        step as ⟨ i3, hi3 ⟩
        step
        case hline =>
          intro c hc
          rw [hl2] at hc
          simp only [List.mem_append, List.mem_singleton] at hc
          rcases hc with h | h
          · exact hline c h
          · subst h
            constructor
            · intro hv; apply h10; scalar_tac
            · intro hv; apply h13; scalar_tac
        exact ⟨‹_›, ‹_›, ‹_›⟩
  · have : i.val = text.length := by scalar_tac
    simp_all
termination_by text.length - i.val
decreasing_by all_goals scalar_decr_tac

/-- `split_lines`: at least one line, and none holds a line break. -/
theorem split_lines_spec (text : Aeneas.Std.Slice U8) (hmax : text.length < Usize.max) :
    split_lines text ⦃ r => 1 ≤ r.val.length ∧ ∀ l ∈ r.val, noBreak l.val ⦄ := by
  unfold split_lines
  step as ⟨ lines, line, hl, hline, hlen ⟩
  · simp [noBreak]
  · simp
  step as ⟨ r, hr ⟩
  refine ⟨by simp [hr], ?_⟩
  intro l hl
  rw [hr] at hl
  simp only [List.mem_append, List.mem_singleton] at hl
  rcases hl with h | h
  · exact hl l h
  · subst h; exact hline

end rope_aeneas
