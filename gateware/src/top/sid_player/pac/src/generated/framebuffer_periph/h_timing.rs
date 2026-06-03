#[doc = "Register `h_timing` reader"]
pub type R = crate::R<H_TIMING_SPEC>;
#[doc = "Register `h_timing` writer"]
pub type W = crate::W<H_TIMING_SPEC>;
#[doc = "Field `h_active` writer - h_active field"]
pub type H_ACTIVE_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
#[doc = "Field `h_sync_start` writer - h_sync_start field"]
pub type H_SYNC_START_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
impl W {
    #[doc = "Bits 0:15 - h_active field"]
    #[inline(always)]
    pub fn h_active(&mut self) -> H_ACTIVE_W<'_, H_TIMING_SPEC> {
        H_ACTIVE_W::new(self, 0)
    }
    #[doc = "Bits 16:31 - h_sync_start field"]
    #[inline(always)]
    pub fn h_sync_start(&mut self) -> H_SYNC_START_W<'_, H_TIMING_SPEC> {
        H_SYNC_START_W::new(self, 16)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`h_timing::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`h_timing::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct H_TIMING_SPEC;
impl crate::RegisterSpec for H_TIMING_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`h_timing::R`](R) reader structure"]
impl crate::Readable for H_TIMING_SPEC {}
#[doc = "`write(|w| ..)` method takes [`h_timing::W`](W) writer structure"]
impl crate::Writable for H_TIMING_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets h_timing to value 0"]
impl crate::Resettable for H_TIMING_SPEC {}
