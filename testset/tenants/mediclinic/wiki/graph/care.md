---
kind: graph
title: Care graph
aliases: [care, 诊疗关系图谱, clinic graph, 预约关系]
summary: Patients and their appointments, the doctors who see them, the departments doctors work in, referrals, and the insurance plans that cover patients.
schema: care
nodes:
  patient:        { en: "A registered patient", zh: "就诊患者", aliases: [patients, 患者, 病人], key: patient_id }
  doctor:         { en: "A doctor", zh: "医生", aliases: [doctors, physician, 医生, 大夫], key: doctor_id }
  department:     { en: "A clinical department", zh: "科室", aliases: [departments, 科室, 门诊], key: dept_code }
  appointment:    { en: "A booked appointment", zh: "预约", aliases: [appointments, booking, visit, 预约, 挂号], key: appt_id }
  insurance_plan: { en: "A commercial insurance plan", zh: "商业保险计划", aliases: [insurance, insurer, 保险, 商保], key: plan_id }
relations:
  booked_by:
    from: appointment
    to: patient
    en: "The patient who booked the appointment"
    zh: "预约的患者"
    aliases: [whose appointment, booked for, 谁的预约, 预约人]
  with_doctor:
    from: appointment
    to: doctor
    en: "The doctor the appointment is with"
    zh: "预约的医生"
    aliases: [seeing, with which doctor, 看哪个医生, 接诊医生]
  works_in:
    from: doctor
    to: department
    en: "The department a doctor works in"
    zh: "医生所在科室"
    aliases: [department of, belongs to, 在哪个科室, 所属科室]
  covered_by:
    from: patient
    to: insurance_plan
    en: "The insurance plan covering the patient"
    zh: "患者的保险计划"
    aliases: [insured by, insurance of, 保的哪家, 参保]
  referred_to:
    from: appointment
    to: department
    en: "A department the appointment referred the patient to"
    zh: "转诊到的科室"
    aliases: [referral, referred, 转诊, 转到]
updated: 2026-09-21
---

# Care graph

An appointment is booked by one patient (`booked_by`) with one doctor (`with_doctor`) and may
refer the patient on (`referred_to`). Doctors work in a department (`works_in`); patients can be
covered by a commercial insurance plan (`covered_by`).
