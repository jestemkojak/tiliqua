#[doc = "Register `resp` reader"]
pub type R = crate::R<RESP_SPEC>;
#[doc = "Register `resp` writer"]
pub type W = crate::W<RESP_SPEC>;
#[doc = "Field `done` reader - done field"]
pub type DONE_R = crate::BitReader;
#[doc = "Field `error` reader - error field"]
pub type ERROR_R = crate::BitReader;
impl R {
    #[doc = "Bit 0 - done field"]
    #[inline(always)]
    pub fn done(&self) -> DONE_R {
        DONE_R::new((self.bits & 1) != 0)
    }
    #[doc = "Bit 1 - error field"]
    #[inline(always)]
    pub fn error(&self) -> ERROR_R {
        ERROR_R::new(((self.bits >> 1) & 1) != 0)
    }
}
impl W {}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`resp::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`resp::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct RESP_SPEC;
impl crate::RegisterSpec for RESP_SPEC {
    type Ux = u8;
}
#[doc = "`read()` method returns [`resp::R`](R) reader structure"]
impl crate::Readable for RESP_SPEC {}
#[doc = "`write(|w| ..)` method takes [`resp::W`](W) writer structure"]
impl crate::Writable for RESP_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets resp to value 0"]
impl crate::Resettable for RESP_SPEC {}
