import Mathlib

-- corpus: abelian-test
theorem abelian_of_forall_mul_self_eq_one
    (G : Type*) [Group G] (h : ∀ g : G, g * g = 1) :
    ∀ a b : G, a * b = b * a := by
  have hs : ∀ g : G, g⁻¹ = g := by
    intro g
    calc
      g⁻¹ = g⁻¹ * 1 := by simp
      _ = g⁻¹ * (g * g) := by rw [← h g]
      _ = (g⁻¹ * g) * g := by rw [mul_assoc]
      _ = 1 * g := by simp
      _ = g := by simp
  intro a b
  calc
    a * b = (a * b)⁻¹ := by
      symm
      exact hs (a * b)
    _ = b⁻¹ * a⁻¹ := by simp
    _ = b * a := by rw [hs b, hs a]

-- corpus: abelian-natural
theorem abelian_of_inv_eq_self {G : Type*} [Group G]
    (h : ∀ x : G, x⁻¹ = x) :
    ∀ a b : G, a * b = b * a := by
  intro a b
  calc
    a * b = (a * b)⁻¹ := by
      simpa using (h (a * b)).symm
    _ = b⁻¹ * a⁻¹ := by
      simp
    _ = b * a := by
      simp [h a, h b]

-- corpus: main_commutation
theorem abelian_of_self_inverse {G : Type*} [Group G]
    (h : ∀ g : G, g = g⁻¹) :
    ∀ a b : G, a * b = b * a := by
  intro a b
  have hab : a * b = (a * b)⁻¹ := by
    simpa using h (a * b)
  have ha : a⁻¹ = a := by
    simpa using (h a).symm
  have hb : b⁻¹ = b := by
    simpa using (h b).symm
  calc
    a * b = (a * b)⁻¹ := hab
    _ = b⁻¹ * a⁻¹ := by simp
    _ = b * a := by simp [ha, hb]

-- corpus: odd-sum-test
lemma odd_sum_succ_step (n : ℕ) :
    n ^ 2 + (2 * n + 1) = (n + 1) ^ 2 := by
  ring

-- corpus: prime-factorial
theorem prime_le_of_dvd_factorial {p n : ℕ} (hp : Nat.Prime p)
    (h : p ∣ n.factorial) : p ≤ n := by
  exact hp.dvd_factorial.mp h

-- corpus: `no_largest_prime`
theorem no_largest_prime (n : ℕ) : ∃ p > n, Nat.Prime p := by
  obtain ⟨p, hp, hprime⟩ := Nat.exists_infinite_primes (n + 1)
  exact ⟨p, lt_of_lt_of_le (Nat.lt_succ_self n) hp, hprime⟩

theorem primes_unbounded : ∀ n : ℕ, ∃ p > n, Nat.Prime p :=
  no_largest_prime

theorem not_exists_largest_prime :
    ¬ ∃ p : ℕ, Nat.Prime p ∧ ∀ q : ℕ, Nat.Prime q → q ≤ p := by
  rintro ⟨p, hp, hmax⟩
  rcases no_largest_prime p with ⟨q, hqgt, hqprime⟩
  exact not_lt_of_ge (hmax q hqprime) hqgt

-- corpus: bertrand
lemma bertrand_one :
    ∃ p, Nat.Prime p ∧ 1 < p ∧ p ≤ 2 := by
  refine ⟨2, by decide, by omega, by omega⟩

-- corpus: one_plus_one_eq_two
theorem one_plus_one_eq_two : 1 + 1 = (2 : Nat) := by
  exact one_add_one_eq_two

-- corpus: isEven
def isEven (n : Nat) : Prop := ∃ k, n = 2 * k

lemma zero_isEven : isEven 0 := ⟨0, by ring⟩

theorem even_plus_even (a b : Nat) (ha : isEven a) (hb : isEven b) : isEven (a + b) := by
  obtain ⟨k, hk⟩ := ha
  obtain ⟨j, hj⟩ := hb
  exact ⟨k + j, by omega⟩

theorem even_times_any (a b : Nat) (ha : isEven a) : isEven (a * b) := by
  obtain ⟨k, hk⟩ := ha
  exact ⟨k * b, by rw [hk]; ring⟩

-- corpus: tui-verify
theorem my_zero_lt_one : (0 : Nat) < 1 := by
  decide

-- corpus: e2e-test
theorem test_omega : ∀ n : Nat, n + 0 = n := by
  simp

theorem test_simp : ∀ (a b : Nat), a + b = b + a := by
  intro a b
  rw [Nat.add_comm]

-- corpus: mcp-smoke2
theorem nat_add_zero (n : Nat) : n + 0 = n := by
  exact Nat.add_zero n

-- corpus: mcp-smoke
theorem add_zero_right (n : Nat) : n + 0 = n := by
  exact Nat.add_zero n

-- corpus: sync-test
theorem false_from_false (h : False) : False := h

-- corpus: one_eq_one
theorem one_eq_one : (1 : Nat) = 1 := by
  rfl

-- corpus: two_irreducible_in_Zsqrtd_neg_five
lemma Zsqrtd_norm_ne_two {z : ℤ√(-5)} : z.norm ≠ 2 := by
  intro hz
  have hdef := Zsqrtd.norm_def (d := -5) z
  rw [hz] at hdef
  have hsum : 2 = z.re * z.re + 5 * (z.im * z.im) := by
    nlinarith [hdef]
  have him_sq_eq_zero : z.im * z.im = 0 := by
    nlinarith [sq_nonneg z.re, sq_nonneg z.im, hsum]
  have him : z.im = 0 := by
    nlinarith [him_sq_eq_zero]
  have hre : z.re * z.re = 2 := by
    nlinarith [hsum, him]
  have hmod : z.re * z.re % 4 = 2 := by
    simpa [hre]
  exact Int.sq_ne_two_mod_four z.re hmod

-- THEOREM

theorem two_irreducible_in_Zsqrtd_neg_five : Irreducible (2 : ℤ√(-5)) := by
  rw [irreducible_iff]
  constructor
  · intro hunit
    have hnormunit : IsUnit ((2 : ℤ√(-5)).norm) :=
      (Zsqrtd.isUnit_iff_norm_isUnit (d := -5) (2 : ℤ√(-5))).1 hunit
    have h4unit : IsUnit (4 : ℤ) := by
      simpa [Zsqrtd.norm_natCast] using hnormunit
    norm_num [Int.isUnit_iff_natAbs_eq] at h4unit
  · intro a b hab
    have hnorm : (2 : ℤ√(-5)).norm = (a * b).norm := by simpa [hab]
    rw [Zsqrtd.norm_mul] at hnorm
    norm_num [Zsqrtd.norm_natCast] at hnorm
    have hnorm4 : 4 = a.norm * b.norm := by
      simpa using hnorm
    have ha_nonneg : 0 ≤ a.norm := Zsqrtd.norm_nonneg (d := -5) (by omega) a
    have hb_nonneg : 0 ≤ b.norm := Zsqrtd.norm_nonneg (d := -5) (by omega) b
    have ha_le : a.norm ≤ 4 := by
      have hb_pos : 0 < b.norm := by
        by_contra hb
        have hb0 : b.norm = 0 := by omega
        simp [hb0] at hnorm4
      have hb_one_le : 1 ≤ b.norm := by
        omega
      nlinarith [hnorm4, hb_one_le]
    have hcases : a.norm = 0 ∨ a.norm = 1 ∨ a.norm = 2 ∨ a.norm = 3 ∨ a.norm = 4 := by
      omega
    rcases hcases with ha0 | ha1 | ha2 | ha3 | ha4
    · exfalso
      simp [ha0] at hnorm4
    · left
      rw [(Zsqrtd.isUnit_iff_norm_isUnit (d := -5) a)]
      rw [ha1]
      exact Int.isUnit_iff_natAbs_eq.mpr rfl
    · exfalso
      exact Zsqrtd_norm_ne_two ha2
    · exfalso
      rw [ha3] at hnorm4
      omega
    · right
      rw [(Zsqrtd.isUnit_iff_norm_isUnit (d := -5) b)]
      have : b.norm = 1 := by
        nlinarith [hnorm4, ha4]
      rw [this]
      exact Int.isUnit_iff_natAbs_eq.mpr rfl

-- TITLE: 2 is irreducible in Z[sqrt(-5)]
-- PROBLEM: Prove that 2 is irreducible in Z[sqrt(-5)]
-- FORMAL_TARGET: Irreducible (2 : ℤ√(-5))
-- ACCEPTED_TARGET: Irreducible (2 : ℤ√(-5))
-- PHASE: skeleton
-- STATUS: compiling

-- corpus: Zsqrtd_norm_ne_two
-- THEOREM

-- corpus: one_plus_one_eq_two_custom
theorem one_plus_one_eq_two_custom : 1 + 1 = (2 : Nat) := by
  rfl

-- corpus: amc12_2000_p12
theorem amc12_2000_p12   (a m c : ℕ)   (h₀ : a + m + c = 12) :
  a*m*c + a*m + m*c + a*c ≤ 112 := by
  have ha : a ≤ 12 := by omega
  have hm : m ≤ 12 := by omega
  interval_cases a <;> interval_cases m <;> omega

-- corpus: amc12a_2002_p6
theorem amc12a_2002_p6 (n : ℕ) (h₀ : 0 < n) : ∃ m, (m > n ∧ ∃ p, m * p ≤ m + p) := by
  refine ⟨n + 1, Nat.lt_succ_self n, ?_⟩
  refine ⟨1, ?_⟩
  simp

-- corpus: amc12a_2020_p4
theorem amc12a_2020_p4
   (S : Finset ℕ)
   (h₀ : ∀ (n : ℕ), n ∈ S ↔ 1000 ≤ n ∧ n ≤ 9999 ∧ (∀ (d : ℕ), d ∈ Nat.digits 10 n → Even d) ∧ 5 ∣ n) :
   S.card = 100 := by
  let T : Finset ℕ :=
    (Finset.range 10000).filter fun n =>
      1000 ≤ n ∧ n ≤ 9999 ∧ (∀ d : ℕ, d ∈ Nat.digits 10 n → Even d) ∧ 5 ∣ n
  have hST : S = T := by
    ext n
    simp [T, h₀ n]
    omega
  rw [hST]
  native_decide

-- corpus: amc12a_2021_p18
theorem amc12a_2021_p18
  (f : ℚ → ℝ)
  (h₀ : ∀ x > 0, ∀ y > 0, f (x * y) = f x + f y)
  (h₁ : ∀ p, Nat.Prime p → f p = p) :
  f (25 / 11) < 0 := by
  have hmul : f ((25 / 11 : ℚ) * 11) = f (25 / 11) + f 11 := by
    apply h₀
    · norm_num
    · norm_num
  have h25 : f (25 : ℚ) = f 5 + f 5 := by
    have := h₀ (5 : ℚ) (by norm_num) (5 : ℚ) (by norm_num)
    norm_num at this ⊢
    simpa using this
  have h5 : f (5 : ℚ) = 5 := h₁ 5 (by norm_num)
  have h11 : f (11 : ℚ) = 11 := h₁ 11 (by norm_num)
  have hcalc : f (25 / 11 : ℚ) = -1 := by
    rw [show ((25 / 11 : ℚ) * 11) = 25 by norm_num] at hmul
    rw [h25] at hmul
    simp [h5, h11] at hmul
    linarith
  linarith

-- corpus: amc12a_2021_p9
theorem amc12a_2021_p9 :
  ∏ k ∈ Finset.range 7, (2^(2^k) + 3^(2^k)) = 3^128 - 2^128 := by
  native_decide

-- corpus: amc12b_2021_p4
theorem amc12b_2021_p4   (m a : ℕ)   (h₀ : 0 < m ∧ 0 < a)   (h₁ : ↑m / ↑a = (3:ℝ) / 4) :   (84 * ↑m + 70 * ↑a) / (↑m + ↑a) = (76:ℝ) := by
  have hm : (0 : ℝ) < m := by exact_mod_cast h₀.1
  have ha : (0 : ℝ) < a := by exact_mod_cast h₀.2
  have ha0 : (a : ℝ) ≠ 0 := by positivity
  have hden : (m : ℝ) + a ≠ 0 := by positivity
  have hrel : (4 : ℝ) * m = 3 * a := by
    have h1' := h₁
    field_simp [ha0] at h1'
    nlinarith
  apply (div_eq_iff hden).2
  nlinarith [hrel]

-- corpus: imo_1960_p2
theorem imo_1960_p2
    (x : ℝ)
    (h₀ : 0 ≤ 1 + 2 * x)
    (h₁ : (1 - Real.sqrt (1 + 2 * x))^2 ≠ 0)
    (h₂ : (4 * x^2) / (1 - Real.sqrt (1 + 2*x))^2 < 2*x + 9)
    (h₃ : x ≠ 0) :
    -(1 / 2) ≤ x ∧ x < 45 / 8 := by
  constructor
  · nlinarith
  · let s : ℝ := Real.sqrt (1 + 2 * x)
    have hsq : s^2 = 1 + 2 * x := by
      dsimp [s]
      simpa using Real.sq_sqrt h₀
    have hdiv : (4 * x^2) / (1 - s)^2 = (1 + s)^2 := by
      apply (div_eq_iff h₁).2
      nlinarith [hsq]
    have hs_lt : s < 7 / 2 := by
      rw [hdiv] at h₂
      nlinarith [h₂, hsq]
    have hs_nonneg : 0 ≤ s := by
      dsimp [s]
      exact Real.sqrt_nonneg _
    have hs_sq_lt : s^2 < (7 / 2 : ℝ)^2 := by
      nlinarith
    nlinarith [hsq, hs_sq_lt]

-- corpus: Prove in Lean 4: theorem mathd_algebra_113   (x : ℝ) :   x^2
theorem mathd_algebra_113 (x : ℝ) :
    x^2 - 14 * x + 3 ≥ 7^2 - 14 * 7 + 3 := by
  nlinarith [sq_nonneg (x - 7)]

-- corpus: mathd_algebra_137
theorem mathd_algebra_137   (x : ℕ)   (h₀ : ↑x + (4:ℝ) / (100:ℝ) * ↑x = 598) :   x = 575 := by
  have hx : (x : ℝ) = 575 := by
    nlinarith [h₀]
  exact_mod_cast hx

-- corpus: mathd_algebra_156
theorem mathd_algebra_156
  (x y : ℝ)
  (f g : ℝ → ℝ)
  (h₀ : ∀ t, f t = t^4)
  (h₁ : ∀ t, g t = 5 * t^2 - 6)
  (h₂ : f x = g x)
  (h₃ : f y = g y)
  (h₄ : x^2 < y^2) :
  y^2 - x^2 = 1 := by
  have hxeq : x^4 = 5 * x^2 - 6 := by
    simpa [h₀ x, h₁ x] using h₂
  have hyeq : y^4 = 5 * y^2 - 6 := by
    simpa [h₀ y, h₁ y] using h₃
  have hxfac : (x^2 - 2) * (x^2 - 3) = 0 := by
    nlinarith [hxeq]
  have hyfac : (y^2 - 2) * (y^2 - 3) = 0 := by
    nlinarith [hyeq]
  have hx_cases : x^2 = 2 ∨ x^2 = 3 := by
    rcases eq_zero_or_eq_zero_of_mul_eq_zero hxfac with hx2 | hx3
    · left
      linarith
    · right
      linarith
  have hy_cases : y^2 = 2 ∨ y^2 = 3 := by
    rcases eq_zero_or_eq_zero_of_mul_eq_zero hyfac with hy2 | hy3
    · left
      linarith
    · right
      linarith
  rcases hx_cases with hx2 | hx3
  · rcases hy_cases with hy2 | hy3
    · nlinarith [h₄, hx2, hy2]
    · nlinarith [h₄, hx2, hy3]
  · rcases hy_cases with hy2 | hy3
    · nlinarith [h₄, hx3, hy2]
    · nlinarith [h₄, hx3, hy3]

-- corpus: zero_add_nat
theorem zero_add_nat (n : Nat) : 0 + n = n := by
  exact Nat.zero_add n

-- corpus: aime_1984_p1
theorem aime_1984_p1
  (u : ℕ → ℚ)
  (h₀ : ∀ n, u (n + 1) = u n + 1)
  (h₁ : ∑ k ∈ Finset.range 98, u k.succ = 137) :
  ∑ k ∈ Finset.range 49, u (2 * k.succ) = 93 := by
  have hu : ∀ n, u n = u 0 + n := by
    intro n
    induction n with
    | zero =>
        norm_num
    | succ n ih =>
        calc
          u n.succ = u n + 1 := by
            simpa [Nat.succ_eq_add_one] using h₀ n
          _ = (u 0 + n) + 1 := by
            rw [ih]
          _ = u 0 + n.succ := by
            norm_num [Nat.succ_eq_add_one, add_assoc, add_left_comm, add_comm]

have hsum98 : (∑ k ∈ Finset.range 98, u k.succ : ℚ) = 98 * u 0 + 4851 := by
    calc
      (∑ k ∈ Finset.range 98, u k.succ : ℚ)
          = ∑ k ∈ Finset.range 98, (u 0 + (k.succ : ℚ)) := by
              apply Finset.sum_congr rfl
              intro k hk
              rw [hu k.succ]
      _ = (∑ k ∈ Finset.range 98, (u 0 : ℚ)) + ∑ k ∈ Finset.range 98, (k.succ : ℚ) := by
            rw [Finset.sum_add_distrib]
      _ = 98 * u 0 + ∑ k ∈ Finset.range 98, (k.succ : ℚ) := by
            simp
      _ = 98 * u 0 + 4851 := by
            norm_num

have hsum49 : (∑ k ∈ Finset.range 49, u (2 * k.succ) : ℚ) = 49 * u 0 + 2450 := by
    calc
      (∑ k ∈ Finset.range 49, u (2 * k.succ) : ℚ)
          = ∑ k ∈ Finset.range 49, (u 0 + (((2 * k.succ : ℕ) : ℚ))) := by
              apply Finset.sum_congr rfl
              intro k hk
              rw [hu (2 * k.succ)]
      _ = (∑ k ∈ Finset.range 49, (u 0 : ℚ)) +
            ∑ k ∈ Finset.range 49, (((2 * k.succ : ℕ) : ℚ)) := by
            rw [Finset.sum_add_distrib]
      _ = 49 * u 0 + ∑ k ∈ Finset.range 49, (((2 * k.succ : ℕ) : ℚ)) := by
            simp
      _ = 49 * u 0 + 2450 := by
            norm_num

linarith [h₁, hsum98, hsum49]

-- corpus: two_irreducible_in_Zsqrtd_neg_five_main
theorem two_irreducible_in_Zsqrtd_neg_five_main : Irreducible (2 : ℤ√(-5)) := by
  exact two_irreducible_in_Zsqrtd_neg_five

-- corpus: sqrt_two_irrational
theorem gauss_quadratic_reciprocity {p q : ℕ} [Fact p.Prime] [Fact q.Prime]
    (hp : p ≠ 2) (hq : q ≠ 2) :
    legendreSym q p = (-1) ^ (p / 2 * (q / 2)) * legendreSym p q := by
  exact legendreSym.quadratic_reciprocity' hp hq

theorem sqrt_two_irrational : Irrational (Real.sqrt 2) := by
  exact irrational_sqrt_two

-- corpus: totalMass
def reducedMass (m₁ m₂ : ℝ) : ℝ := m₁ * m₂ / (m₁ + m₂)

def lagrangian
    (m₁ m₂ k b Xdot Ydot rdot θdot r : ℝ) : ℝ :=
  (totalMass m₁ m₂ / 2) * (Xdot ^ 2 + Ydot ^ 2) +
    (reducedMass m₁ m₂ / 2) * (rdot ^ 2 + r ^ 2 * θdot ^ 2) -
    (k / 2) * (r - b) ^ 2

def xEL (m₁ m₂ Xddot : ℝ) : ℝ := totalMass m₁ m₂ * Xddot

def yEL (m₁ m₂ Yddot : ℝ) : ℝ := totalMass m₁ m₂ * Yddot

def radialEL (m₁ m₂ k b r rddot θdot : ℝ) : ℝ :=
  reducedMass m₁ m₂ * rddot - (reducedMass m₁ m₂ * r * θdot ^ 2 - k * (r - b))

def angularEL (m₁ m₂ r rdot θdot θddot : ℝ) : ℝ :=
  reducedMass m₁ m₂ * (r ^ 2 * θddot + 2 * r * rdot * θdot)

theorem lagrangian_equations_of_motion
    (m₁ m₂ k b Xddot Yddot r rdot rddot θdot θddot : ℝ) :
    xEL m₁ m₂ Xddot = 0 ∧
      yEL m₁ m₂ Yddot = 0 ∧
      radialEL m₁ m₂ k b r rddot θdot = 0 ∧
      angularEL m₁ m₂ r rdot θdot θddot = 0 ↔
    totalMass m₁ m₂ * Xddot = 0 ∧
      totalMass m₁ m₂ * Yddot = 0 ∧
      reducedMass m₁ m₂ * rddot = reducedMass m₁ m₂ * r * θdot ^ 2 - k * (r - b) ∧
      reducedMass m₁ m₂ * (r ^ 2 * θddot + 2 * r * rdot * θdot) = 0 := by
  constructor
  · intro h
    rcases h with ⟨hx, hy, hr, hθ⟩
    refine ⟨?_, ?_, ?_, ?_⟩
    · exact hx
    · exact hy
    · unfold radialEL at hr
      linarith
    · simpa [angularEL] using hθ
  · intro h
    rcases h with ⟨hx, hy, hr, hθ⟩
    refine ⟨?_, ?_, ?_, ?_⟩
    · exact hx
    · exact hy
    · unfold radialEL
      linarith
    · simpa [angularEL] using hθ

def totalMass (m₁ m₂ : ℝ) : ℝ := m₁ + m₂

-- corpus: fib-v3
lemma gap_three_of_ratio_three_halves {a b c : ℕ}
    (ha : 3 ≤ a) (hab : 3 * a ≤ 2 * b) (hbc : 3 * b ≤ 2 * c) :
    a + 3 ≤ c := by
  omega

